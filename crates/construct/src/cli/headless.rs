//! `construct run`: one prompt without a terminal. Events print as they happen
//! (`--json` prints them all); the run is over when the turn is and, after it,
//! every background job it left has ended and had its turn.

use crate::agent::{Agent, Event};
use crate::config::{Config, Paths};
use crate::{learn, llm};
use anyhow::Result;
use std::io::Write;

pub(super) async fn run(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    text: String,
    json: bool,
    learn: bool,
) -> Result<()> {
    let learn = learn && cfg.learn.enabled;
    let mut cfg = cfg;
    llm::overlay::prepare(&mut cfg, &paths).await;
    let cwd = std::env::current_dir()?;
    let mut agent = Agent::new(cfg, paths, cwd, resume)?;
    // Overlaps the model's TLS handshake with routing.
    agent.warm();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let printer = tokio::spawn(async move {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut failed = false;
        let mut at_line_start = true;
        while let Some(ev) = rx.recv().await {
            if json {
                let _ = writeln!(out, "{}", serde_json::to_string(&ev).unwrap_or_default());
                let _ = out.flush();
                if matches!(ev, Event::Error { .. }) {
                    failed = true;
                }
                continue;
            }
            match ev {
                Event::Text { text } => {
                    let _ = write!(out, "{text}");
                    let _ = out.flush();
                    at_line_start = text.ends_with('\n');
                }
                Event::ToolStart { name, args, .. } => {
                    if !at_line_start {
                        let _ = writeln!(out);
                        at_line_start = true;
                    }
                    let _ = writeln!(err, "· {name} {}", llm::truncate(&args.replace('\n', " "), 160));
                }
                Event::ToolEnd { ok: false, output, .. } => {
                    let _ = writeln!(err, "  ✗ {}", llm::truncate(&output.replace('\n', " "), 200));
                }
                Event::Error { message } => {
                    failed = true;
                    let _ = writeln!(err, "error: {message}");
                }
                Event::TurnEnd { .. } if !at_line_start => {
                    let _ = writeln!(out);
                }
                _ => {}
            }
        }
        failed
    });
    let cancel = agent.canceller();
    let (turn, mut interrupted) = interruptible(agent.turn(&text, &tx), &cancel).await;
    turn?;
    // Jobs the turn left running: wait for each ending and give it a turn, so
    // the run is not over while work it asked for is. The jobs' own time
    // limits bound this, and Ctrl-C cancels them.
    while !interrupted {
        let next = tokio::select! {
            n = agent.next_wake() => n,
            _ = tokio::signal::ctrl_c() => break,
        };
        if !next {
            break;
        }
        let (woke, stopped) = interruptible(agent.wake(&tx), &cancel).await;
        woke?;
        interrupted = stopped;
    }
    // Ctrl-C (or the end of the run) leaves nothing running, and the journal
    // says so.
    if interrupted || agent.jobs().running() > 0 {
        agent.stop_jobs().await;
    }
    drop(tx);
    let failed = printer.await.unwrap_or(true);
    if learn && learn::kind_for(agent.user_turns, agent.tool_calls).is_some() {
        learn::spawn_detached(&agent.journal.session);
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Run a turn that Ctrl-C interrupts rather than kills: the turn is asked to
/// stop and still ends properly, so the journal records it as interrupted.
/// Whether it was interrupted comes back with its result.
async fn interruptible<T>(
    turn: impl std::future::Future<Output = Result<T>>,
    cancel: &tokio::sync::watch::Sender<bool>,
) -> (Result<T>, bool) {
    tokio::pin!(turn);
    let mut interrupted = false;
    loop {
        tokio::select! {
            r = &mut turn => return (r, interrupted),
            _ = tokio::signal::ctrl_c(), if !interrupted => {
                cancel.send_replace(true);
                interrupted = true;
            }
        }
    }
}
