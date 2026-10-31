//! The agent side of the TUI: builds the agent off the UI path, runs turns
//! from commands, and starts the learning pass.

use super::Cmd;
use super::ask::{Prompt, TuiAsk};
use crate::agent::{Agent, Event};
use crate::config::{Config, Paths};
use crate::learn;
use crate::tools::Catalog;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// What the agent task hands the UI once it is built: session id, turn
/// canceller, enabled groups, resumed history, the session's tools and its jobs
/// (so the UI can cancel one while a turn runs).
pub(super) type Ready = (
    String,
    watch::Sender<bool>,
    Vec<String>,
    Vec<crate::llm::Message>,
    Arc<Catalog>,
    crate::journal::Totals,
    crate::jobs::Jobs,
);

pub(super) async fn agent_task(
    mut cfg: Config,
    paths: Paths,
    resume: Option<String>,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    tx: mpsc::UnboundedSender<Event>,
    ready: mpsc::UnboundedSender<Ready>,
    prompts: mpsc::UnboundedSender<Prompt>,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    // Off the UI path: the first frame is up, and this waits a few seconds at most.
    crate::llm::overlay::prepare(&mut cfg, &paths).await;
    let build = |resume: Option<String>, cfg: Config| Agent::new(cfg, paths.clone(), cwd.clone(), resume);
    let mut agent = match build(resume, cfg.clone()) {
        Ok(a) => a,
        Err(e) => {
            let _ = tx.send(Event::Error { message: format!("fatal: {e:#}") });
            return;
        }
    };
    crate::trace::mark("agent ready");
    agent.ctx.perms.set_ask(Arc::new(TuiAsk { tx: prompts.clone() }));
    agent.warm();
    let history: Vec<crate::llm::Message> = agent.messages.iter().skip(1).cloned().collect();
    // A resumed session's status line starts from what it has already spent.
    let spent = if history.is_empty() { Default::default() } else { spent(&paths, &cwd, &agent.journal.session) };
    let _ = ready.send((
        agent.journal.session.clone(),
        agent.canceller(),
        vec![],
        history,
        agent.catalog.clone(),
        spent,
        agent.jobs(),
    ));
    // How far learning has seen: an index into the live conversation, and
    // the journal seq the detached learner resumes from.
    let mut reviewed = (agent.messages.len(), agent.journal.last_seq());
    loop {
        // A command the user already typed goes first; a job that ended while
        // idle starts a turn of its own (ADR 0026).
        let jobs = agent.jobs();
        let c = tokio::select! {
            biased;
            c = cmds.recv() => match c {
                Some(c) => c,
                None => break,
            },
            () = jobs.ready() => {
                let _ = agent.wake(&tx).await;
                maybe_learn(&mut agent, &tx, &mut reviewed, false);
                continue;
            }
        };
        match c {
            Cmd::Prompt(text) => {
                let _ = agent.turn(&text, &tx).await;
                maybe_learn(&mut agent, &tx, &mut reviewed, false);
            }
            Cmd::Skill { name, text } => {
                let _ = agent.invoke(&name, &text, &tx).await;
                maybe_learn(&mut agent, &tx, &mut reviewed, false);
            }
            Cmd::LearnNow => maybe_learn(&mut agent, &tx, &mut reviewed, true),
            Cmd::Model(m) => {
                if let Err(e) = agent.set_model(&m, &tx) {
                    let _ = tx.send(Event::Error { message: format!("model {m}: {e:#}") });
                }
            }
            Cmd::New => {
                agent.stop_jobs().await;
                finish(&agent, reviewed);
                match build(None, agent.cfg.clone()) {
                    Ok(a) => {
                        agent = a;
                        agent.ctx.perms.set_ask(Arc::new(TuiAsk { tx: prompts.clone() }));
                        reviewed = (agent.messages.len(), agent.journal.last_seq());
                        let _ = ready.send((
                            agent.journal.session.clone(),
                            agent.canceller(),
                            vec![],
                            vec![],
                            agent.catalog.clone(),
                            Default::default(),
                            agent.jobs(),
                        ));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error { message: format!("{e:#}") });
                    }
                }
            }
        }
    }
    // Quitting cancels what is still running, and the journal says so; the
    // helpers' own watchdog covers a process that is stopped before this ends.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(1200), agent.stop_jobs()).await;
    finish(&agent, reviewed);
}

/// Hand any unreviewed tail of the session to a detached learning process.
fn finish(agent: &Agent, (reviewed, seq): (usize, i64)) {
    if !agent.cfg.learn.enabled || reviewed >= agent.messages.len() {
        return;
    }
    let tail = &agent.messages[reviewed..];
    let users = tail.iter().filter(|m| m.role == "user").count() as u32;
    let calls = tail.iter().map(|m| m.tool_calls.len() as u32).sum();
    if learn::kind_for(users, calls).is_some() {
        let mut st = learn::state(&agent.paths);
        st["reviewed_seq"][&agent.journal.session] = serde_json::json!(seq);
        learn::save_state(&agent.paths, &st);
        learn::spawn_detached(&agent.journal.session);
    }
}

fn maybe_learn(agent: &mut Agent, tx: &mpsc::UnboundedSender<Event>, reviewed: &mut (usize, i64), force: bool) {
    if !agent.cfg.learn.enabled && !force {
        return;
    }
    let mem = agent.turns_since_memory >= agent.cfg.learn.memory_every_turns;
    let skill = agent.tool_calls_since_skill >= agent.cfg.learn.skill_every_tool_calls;
    let kind = match (mem || force, skill || force) {
        (true, true) => learn::Kind::Both,
        (true, false) => learn::Kind::Memory,
        (false, true) => learn::Kind::Skills,
        _ => return,
    };
    if reviewed.0 >= agent.messages.len() {
        return;
    }
    agent.turns_since_memory = 0;
    agent.tool_calls_since_skill = 0;
    let slice = agent.messages[reviewed.0..].to_vec();
    *reviewed = (agent.messages.len(), agent.journal.last_seq());
    let (cfg, paths, tx, id, upto) =
        (agent.cfg.clone(), agent.paths.clone(), tx.clone(), agent.journal.session.clone(), reviewed.1);
    tokio::spawn(async move {
        let _ = tx.send(Event::Learn { status: "learning from this session".into() });
        if let Some(r) = learn::distill(&cfg, &paths, &id, &slice).await {
            let status = match r {
                Ok(o) => {
                    learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
                    if o.actions.is_empty() { "learned nothing new".to_string() } else { o.summary }
                }
                Err(e) => format!("learning failed: {e}"),
            };
            let mut st = learn::state(&paths);
            st["reviewed_seq"][&id] = serde_json::json!(upto);
            learn::save_state(&paths, &st);
            let _ = tx.send(Event::Learn { status });
            return;
        }
        let gate = learn::gate(&cfg, &slice).await;
        learn::log_line(&paths, &format!("session {id} (live): gate {} {} -> {:?}", gate.source, gate.note, gate.kind));
        // The counters said "time to look"; the gate says whether there's anything to see.
        let kind = match (gate.kind, force) {
            (Some(k), _) => k,
            (None, true) => kind,
            (None, false) => {
                let mut st = learn::state(&paths);
                st["reviewed_seq"][&id] = serde_json::json!(upto);
                learn::save_state(&paths, &st);
                let _ = tx.send(Event::Learn { status: "nothing new to learn".into() });
                return;
            }
        };
        let status = match learn::review(&cfg, &paths, &id, &slice, kind).await {
            Ok(o) => {
                learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
                if o.actions.is_empty() {
                    "learned nothing new".to_string()
                } else {
                    format!(
                        "learned: {}",
                        o.actions.iter().map(|a| a.split(':').next().unwrap_or("")).collect::<Vec<_>>().join(", ")
                    )
                }
            }
            Err(e) => format!("learning failed: {e}"),
        };
        let mut st = learn::state(&paths);
        st["reviewed_seq"][&id] = serde_json::json!(upto);
        learn::save_state(&paths, &st);
        let _ = tx.send(Event::Learn { status });
    });
}

/// What `session` has spent so far, summed from the journal as `/status` does.
fn spent(paths: &Paths, cwd: &std::path::Path, session: &str) -> crate::journal::Totals {
    let read = || -> anyhow::Result<_> {
        let db = crate::journal::Journal::open(&crate::journal::path(paths, cwd))?;
        crate::journal::Totals::session(&db, &db.since(session, 0)?)
    };
    read().unwrap_or_default()
}
