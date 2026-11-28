//! One turn producer. Its completion follows the drained event stream on the
//! server command channel, so the next queued prompt cannot overtake it.

use super::ask::{Answer, AskMsg, ClientAsk};
use super::keys::{Outcome, Waiter};
use crate::agent::Agent;
use crate::event::Event;
use crate::permission::Request;
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
}

pub(super) async fn run(
    agent: Arc<Mutex<Agent>>,
    session: String,
    prompt: Prompt,
    commands: mpsc::UnboundedSender<Command>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let (questions, mut asks) = mpsc::unbounded_channel::<AskMsg>();
    let generation = prompt.generation;
    let forward_ask = |msg: AskMsg| match msg {
        AskMsg::Ask { key, request, answer } => {
            Command::Ask { key, session: session.clone(), generation, request, answer }
        }
        AskMsg::Forget(key) => Command::Forget(key),
    };
    let forward = async {
        let (mut interrupted, mut error, mut max_steps) = (false, None::<String>, false);
        loop {
            // Tools emit their start before asking permission. Drain those
            // events first, even when the asker runs on another worker.
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
    };
    let turn = async {
        let mut agent = agent.lock().await;
        agent.set_permission_ask(Arc::new(ClientAsk::new(questions)));
        let result = agent.turn_keyed(&prompt.text, prompt.key.clone(), &tx).await;
        drop(tx);
        result
    };
    let (result, (interrupted, error, max_steps)) = tokio::join!(turn, forward);
    let result = match (result, interrupted, error) {
        (Err(e), ..) => Err(format!("{e:#}")),
        (_, true, _) => Ok("cancelled"),
        (_, _, Some(_)) if max_steps => Ok("max_turn_requests"),
        (_, _, Some(e)) => Err(e),
        _ => Ok("end_turn"),
    };
    let _ = commands.send(Command::Finished { session, prompt, result });
}
