//! One turn producer. Its completion follows the drained event stream on the
//! server command channel, so the next queued prompt cannot overtake it.

use super::ask::ClientAsk;
use super::live::Command;
use crate::agent::Agent;
use crate::event::Event;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

pub(super) struct Prompt {
    pub generation: u64,
    pub id: Value,
    pub attachment: u64,
    pub text: String,
}

pub(super) async fn run(
    agent: Arc<Mutex<Agent>>,
    session: String,
    prompt: Prompt,
    commands: mpsc::UnboundedSender<Command>,
    ask_client: bool,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let (questions, mut asks) = mpsc::unbounded_channel();
    let forward = async {
        let (mut interrupted, mut error, mut max_steps) = (false, None::<String>, false);
        loop {
            // Tools emit their start before asking permission. Drain those
            // events first, even when the asker runs on another worker.
            let event = tokio::select! {
                biased;
                event = rx.recv() => match event { Some(event) => event, None => break },
                Some(command) = asks.recv() => {
                    let _ = commands.send(command);
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
            let _ = commands.send(Command::Event {
                session: session.clone(),
                generation: prompt.generation,
                event: Box::new(event),
            });
        }
        while let Ok(command) = asks.try_recv() {
            let _ = commands.send(command);
        }
        (interrupted, error, max_steps)
    };
    let turn = async {
        let mut agent = agent.lock().await;
        if ask_client {
            agent.ctx.perms.set_ask(Arc::new(ClientAsk::new(session.clone(), prompt.generation, questions)));
        }
        let result = agent.turn(&prompt.text, &tx).await;
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
