//! The agent side of the TUI. Build a complete destination before replacing
//! the active runtime, and tag asynchronous events with their owning session.

use super::app::Cmd;
use super::ask::{Prompt, TuiAsk};
use super::learning::{finish, maybe_learn};
use crate::agent::{Agent, Event, Events};
use crate::config::{Config, Paths};
use crate::journal::{Entry, Journal, Totals};
use std::sync::Arc;
use tokio::sync::mpsc;

pub(super) struct Ready {
    pub session: String,
    pub tag: String,
    pub ack: Option<tokio::sync::oneshot::Sender<()>>,
    pub canceller: tokio::sync::watch::Sender<bool>,
    pub entries: Vec<Entry>,
    pub catalog: Arc<crate::tools::Catalog>,
    pub spent: Totals,
    pub jobs: crate::jobs::Jobs,
    pub cfg: Config,
    pub groups: Vec<String>,
}

impl Ready {
    fn of(a: &Agent) -> anyhow::Result<Self> {
        let mut entries = a.journal.entries()?;
        let db = Journal::read_only(&crate::journal::path(&a.paths, &a.ctx.cwd))?;
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let tag = format!("{}:{}", a.journal.session, GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let spent = Totals::session(&db, &entries)?;
        entries = super::restore::with_children(entries, &db)?;
        Ok(Self {
            ack: None,
            tag,
            session: a.journal.session.clone(),
            canceller: a.canceller(),
            catalog: a.catalog.clone(),
            spent,
            entries,
            jobs: a.jobs(),
            cfg: a.cfg.clone(),
            groups: a
                .groups
                .iter()
                .filter(|g| **g != crate::tools::Group::Core)
                .map(crate::tools::Group::label)
                .collect(),
        })
    }
}

pub(super) struct Out {
    pub tx: mpsc::UnboundedSender<(String, Event)>,
    pub ready: mpsc::UnboundedSender<Ready>,
    pub prompts: mpsc::UnboundedSender<Prompt>,
    pub landed: mpsc::UnboundedSender<()>,
    /// A prompt or `/skill` command answered without a turn.
    pub refused: mpsc::UnboundedSender<()>,
}

fn session_events(id: String, out: mpsc::UnboundedSender<(String, Event)>) -> Events {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = out.send((id.clone(), ev));
        }
    });
    tx
}

pub(super) async fn agent_task(
    mut cfg: Config,
    paths: Paths,
    resume: Option<String>,
    startup_browser: bool,
    model_override: Option<String>,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    out: Out,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut active: Option<(Agent, Events)> = None;
    let mut pending = resume.clone();
    let mut reviewed = (0, 0);
    let mut first = !startup_browser;
    loop {
        // Startup browsing does not resolve credentials or ask providers.
        let command = if first {
            first = false;
            Cmd::Resume(resume.clone())
        } else {
            let jobs = active.as_ref().map(|(a, _)| a.jobs());
            tokio::select! {
                biased;
                c = cmds.recv() => match c { Some(c) => c, None => break },
                () = async {
                    match jobs { Some(j) => j.ready().await, None => std::future::pending().await }
                } => {
                    if let Some((a, tx)) = &mut active {
                        let _ = a.wake(tx).await;
                        maybe_learn(a, tx, &mut reviewed, false);
                    }
                    continue;
                }
            }
        };
        match command {
            Cmd::Resume(id) => {
                if active.as_ref().is_some_and(|(a, _)| id.as_deref() == Some(a.journal.session.as_str())) {
                    continue;
                }
                if active.as_ref().is_some_and(|(a, _)| a.jobs().running() > 0) {
                    error(&out, &active, "Cannot switch while a background job is active; wait or cancel it first.");
                    continue;
                }
                pending = id.clone();
                crate::llm::overlay::prepare(&mut cfg, &paths, Some(out.landed.clone())).await;
                match prepare(&cfg, &paths, &cwd, id, model_override.as_deref()) {
                    Ok((a, ready)) => {
                        if let Some((old, _)) = &active {
                            finish(old, reviewed);
                        }
                        a.ctx.perms.set_ask(Arc::new(TuiAsk { tx: out.prompts.clone() }));
                        let tx = session_events(ready.tag.clone(), out.tx.clone());
                        reviewed = (a.messages.len(), a.journal.last_seq());
                        a.warm();
                        publish(&out, ready).await;
                        active = Some((a, tx));
                        pending = None;
                    }
                    Err(e) => error(
                        &out,
                        &active,
                        &format!(
                            "Could not restore session: {e:#}. Use /model to choose a model if unavailable, then retry /resume."
                        ),
                    ),
                }
            }
            Cmd::Model(m) if active.is_none() || pending.is_some() => {
                match prepare(&cfg, &paths, &cwd, pending.clone(), Some(&m)) {
                    Ok((a, ready)) => {
                        if let Some((old, _)) = &active {
                            finish(old, reviewed);
                        }
                        a.ctx.perms.set_ask(Arc::new(TuiAsk { tx: out.prompts.clone() }));
                        let tx = session_events(ready.tag.clone(), out.tx.clone());
                        reviewed = (a.messages.len(), a.journal.last_seq());
                        a.warm();
                        publish(&out, ready).await;
                        active = Some((a, tx));
                        pending = None;
                    }
                    Err(e) => error(&out, &active, &format!("Could not restore with model {m}: {e:#}")),
                }
            }
            Cmd::Prompt(text) => {
                pending = None;
                if let Some((a, tx)) = &mut active {
                    let _ = a.turn(&text, tx).await;
                    maybe_learn(a, tx, &mut reviewed, false);
                } else {
                    error(&out, &active, "No active session; use /model to choose an available model, or /resume.");
                    let _ = out.refused.send(());
                }
            }
            Cmd::Skill { name, text, original } => {
                let ran = match &mut active {
                    Some((a, tx)) => {
                        let ran = a.invoke_as(&name, &text, &original, tx).await.unwrap_or(true);
                        maybe_learn(a, tx, &mut reviewed, false);
                        ran
                    }
                    None => false,
                };
                if !ran {
                    let _ = out.refused.send(());
                }
            }
            Cmd::LearnNow => {
                if let Some((a, tx)) = &mut active {
                    maybe_learn(a, tx, &mut reviewed, true);
                }
            }
            Cmd::Model(m) => {
                if let Some((a, tx)) = &mut active
                    && let Err(e) = a.set_model(&m, tx)
                {
                    error(&out, &active, &format!("model {m}: {e:#}"));
                }
            }
            Cmd::Rename(title) => {
                if let Some((a, tx)) = &active {
                    match a.journal.rename(&title) {
                        Ok(()) => {
                            let _ = tx.send(Event::Learn { status: format!("Session renamed: {}", title.trim()) });
                        }
                        Err(e) => error(&out, &active, &format!("Could not rename: {e:#}")),
                    }
                }
            }
            Cmd::New => {
                // Serialized through this worker just like resume.
                if let Some((a, _)) = &active {
                    a.stop_jobs().await;
                }
                let current_cfg = active.as_ref().map(|(a, _)| &a.cfg).unwrap_or(&cfg);
                match prepare(current_cfg, &paths, &cwd, None, None) {
                    Ok((a, ready)) => {
                        if let Some((old, _)) = &active {
                            finish(old, reviewed);
                        }
                        a.ctx.perms.set_ask(Arc::new(TuiAsk { tx: out.prompts.clone() }));
                        let tx = session_events(ready.tag.clone(), out.tx.clone());
                        reviewed = (a.messages.len(), a.journal.last_seq());
                        a.warm();
                        publish(&out, ready).await;
                        active = Some((a, tx));
                        pending = None;
                    }
                    Err(e) => error(&out, &active, &format!("Could not start session: {e:#}")),
                }
            }
        }
    }
    if let Some((a, _)) = &active {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(1200), a.stop_jobs()).await;
        finish(a, reviewed);
    }
}

fn prepare(
    cfg: &Config,
    paths: &Paths,
    cwd: &std::path::Path,
    id: Option<String>,
    model_override: Option<&str>,
) -> anyhow::Result<(Agent, Ready)> {
    let a = Agent::new_with_model(cfg.clone(), paths.clone(), cwd.to_path_buf(), id, model_override)?;
    crate::trace::mark("agent ready");
    let ready = Ready::of(&a)?;
    Ok((a, ready))
}

fn error(out: &Out, active: &Option<(Agent, Events)>, message: &str) {
    let ev = Event::Error { message: message.to_string() };
    if let Some((_, tx)) = active {
        let _ = tx.send(ev);
    } else {
        let _ = out.tx.send((String::new(), ev));
    }
}

async fn publish(out: &Out, mut ready: Ready) {
    let (ack, landed) = tokio::sync::oneshot::channel();
    ready.ack = Some(ack);
    if out.ready.send(ready).is_ok() {
        let _ = landed.await;
    }
}
