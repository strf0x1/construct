//! One turn producer. Its completion follows the drained event stream on the
//! server command channel, so the next queued prompt cannot overtake it.

use super::ask::{Answer, AskMsg, ClientAsk};
use super::keys::{Outcome, Waiter};
use crate::agent::Agent;
use crate::event::Event;
use crate::permission::Request;
use crate::wake::WakeEvent;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

pub(super) struct Prompt {
    pub generation: u64,
    pub id: Value,
    pub attachment: u64,
    pub text: String,
    /// The client's idempotency key, and resends of it waiting on this prompt.
    pub key: Option<String>,
    pub waiters: Vec<Waiter>,
}

/// Messages a running turn task sends back to the server actor.
pub(super) enum Command {
    Event { session: String, generation: u64, event: Box<Event> },
    Ask { key: u64, session: String, generation: u64, request: Request, answer: oneshot::Sender<Answer> },
    Forget(u64),
    Finished { session: String, prompt: Prompt, result: Outcome },
    WakeFinished { session: String, generation: u64, result: Outcome },
    JobReady { session: String, generation: u64 },
}

pub(super) async fn run(
    agent: Arc<Mutex<Agent>>,
    session: String,
    prompt: Prompt,
    commands: mpsc::UnboundedSender<Command>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let (questions, asks) = mpsc::unbounded_channel::<AskMsg>();
    let generation = prompt.generation;
    let forward = forward_stream(rx, asks, session.clone(), generation, commands.clone());
    let turn = async {
        let mut agent = agent.lock().await;
        agent.set_permission_ask(Arc::new(ClientAsk::new(questions)));
        let result = agent.turn_keyed(&prompt.text, prompt.key.clone(), &tx).await;
        drop(tx);
        result
    };
    let (result, stats) = tokio::join!(turn, forward);
    let outcome = to_outcome(result, stats);
    let _ = commands.send(Command::Finished { session, prompt, result: outcome });
}

/// Run a wake turn (either external `WakeEvent`s or pending background job
/// completions when `events` is empty) on an ACP session.
pub(super) async fn run_wake(
    agent: Arc<Mutex<Agent>>,
    session: String,
    generation: u64,
    events: Vec<WakeEvent>,
    commands: mpsc::UnboundedSender<Command>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let (questions, asks) = mpsc::unbounded_channel::<AskMsg>();
    let forward = forward_stream(rx, asks, session.clone(), generation, commands.clone());
    let turn = async {
        let mut agent = agent.lock().await;
        agent.set_permission_ask(Arc::new(ClientAsk::new(questions)));
        let result = if events.is_empty() {
            agent.wake(&tx).await.map(|_| ())
        } else {
            agent.wake_with(events, &tx).await.map(|_| ())
        };
        drop(tx);
        result
    };
    let (result, stats) = tokio::join!(turn, forward);
    let outcome = to_outcome(result, stats);
    let _ = commands.send(Command::WakeFinished { session, generation, result: outcome });
}

async fn forward_stream(
    mut rx: mpsc::UnboundedReceiver<Event>,
    mut asks: mpsc::UnboundedReceiver<AskMsg>,
    session: String,
    generation: u64,
    commands: mpsc::UnboundedSender<Command>,
) -> (bool, Option<String>, bool) {
    let forward_ask = |msg: AskMsg| match msg {
        AskMsg::Ask { key, request, answer } => {
            Command::Ask { key, session: session.clone(), generation, request, answer }
        }
        AskMsg::Forget(key) => Command::Forget(key),
    };
    let (mut interrupted, mut error, mut max_steps) = (false, None::<String>, false);
    loop {
        let event = tokio::select! {
            biased;
            event = rx.recv() => match event { Some(event) => event, None => break },
            Some(msg) = asks.recv() => {
                let _ = commands.send(forward_ask(msg));
                continue;
            }
        };
        match &event {
            Event::TurnEnd { interrupted: i, .. } => interrupted = *i,
            Event::Error { message } => {
                max_steps |= message.ends_with("(max_steps)");
                error = Some(message.clone());
            }
            _ => {}
        }
        let _ = commands.send(Command::Event { session: session.clone(), generation, event: Box::new(event) });
    }
    while let Ok(msg) = asks.try_recv() {
        let _ = commands.send(forward_ask(msg));
    }
    (interrupted, error, max_steps)
}

fn to_outcome(result: anyhow::Result<()>, (interrupted, error, max_steps): (bool, Option<String>, bool)) -> Outcome {
    match (result, interrupted, error) {
        (Err(e), ..) => Err(format!("{e:#}")),
        (_, true, _) => Ok("cancelled"),
        (_, _, Some(_)) if max_steps => Ok("max_turn_requests"),
        (_, _, Some(e)) => Err(e),
        _ => Ok("end_turn"),
    }
}
