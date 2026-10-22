//! The child's side: `construct __child`. Read `start`, build an agent that
//! runs as the delegated session, then run each `prompt` as a turn while
//! watching stdin for `cancel`. Every event goes to stdout as a line. Stdin
//! closing ends the child, after cancelling any turn still running.

use super::wire::{ToChild, event_line};
use crate::agent::{Agent, Event};
use crate::config::Paths;
use anyhow::{Result, bail};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};

pub async fn run() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let Some(ToChild::Start { config, cwd, origin }) = lines.next_line().await?.as_deref().and_then(ToChild::parse)
    else {
        bail!("expected a start line on stdin");
    };
    let paths = Paths::resolve()?;
    let mut agent = Agent::child(*config, paths, cwd, *origin)?;
    let cancel = agent.canceller();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let printer = tokio::task::spawn_blocking(move || {
        let mut out = std::io::stdout();
        while let Some(ev) = rx.blocking_recv() {
            // A parent that stopped reading has stopped caring.
            if out.write_all(event_line(&ev).as_bytes()).and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    });
    let mut open = true;
    while open && let Some(line) = lines.next_line().await? {
        let Some(ToChild::Prompt { text }) = ToChild::parse(&line) else { continue };
        let turn = agent.turn(&text, &tx);
        tokio::pin!(turn);
        loop {
            tokio::select! {
                r = &mut turn => {
                    if let Err(e) = r {
                        let _ = tx.send(Event::Error { message: format!("{e:#}") });
                    }
                    break;
                }
                next = lines.next_line(), if open => match next? {
                    // Stdin closed mid-turn: stop it, let it wrap up, then leave.
                    None => {
                        open = false;
                        cancel.send_replace(true);
                    }
                    Some(l) => {
                        if let Some(ToChild::Cancel) = ToChild::parse(&l) {
                            cancel.send_replace(true);
                        }
                    }
                },
            }
        }
    }
    drop(tx);
    let _ = printer.await;
    Ok(())
}
