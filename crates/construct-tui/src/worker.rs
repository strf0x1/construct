//! The agent side of the TUI. Build a complete destination before replacing
//! the active runtime, and tag asynchronous events with their owning session.

use crate::overlays::ask::{Prompt, TuiAsk};
use construct_core::agent::{Agent, Event, Events};
use construct_core::config::{Config, Paths};
use construct_core::journal::{Entry, Totals};
use construct_core::tools::ToolSources;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) enum Cmd {
    Prompt(String),
    /// `/<skill> text`: run a turn with the skill loaded.
    Skill {
        name: String,
        text: String,
        original: String,
    },
    New,
    Resume(Option<String>),
    Rename(String),
    Model(String),
    LearnNow,
}

pub(crate) struct Ready {
    pub(crate) session: String,
    pub(crate) tag: String,
    pub(crate) ack: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) canceller: tokio::sync::watch::Sender<bool>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) catalog: Arc<construct_core::tools::Catalog>,
    pub(crate) spent: Totals,
    pub(crate) jobs: construct_core::jobs::Jobs,
    pub(crate) cfg: Config,
    pub(crate) groups: Vec<String>,
}

impl Ready {
    fn of(a: &Agent) -> anyhow::Result<Self> {
        let entries = a.entries()?;
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let session = a.session_id().to_string();
        let tag = format!("{session}:{}", GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let spent = Totals::of(&entries);
        Ok(Self {
            ack: None,
            tag,
            session,
            canceller: a.canceller(),
            catalog: a.catalog(),
            spent,
            entries,
            jobs: a.jobs(),
            cfg: a.config().clone(),
            groups: a.active_group_labels(),
        })
    }
}

pub(crate) struct Out {
    pub(crate) tx: mpsc::UnboundedSender<(String, Event)>,
    pub(crate) ready: mpsc::UnboundedSender<Ready>,
    pub(crate) prompts: mpsc::UnboundedSender<Prompt>,
    pub(crate) landed: mpsc::UnboundedSender<()>,
    /// A prompt or `/skill` command answered without a turn.
    pub(crate) refused: mpsc::UnboundedSender<()>,
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

/// How this run started: what to resume, how, and the tools the binary adds.
pub(crate) struct Launch {
    pub(crate) resume: Option<String>,
    pub(crate) startup_browser: bool,
    pub(crate) model_override: Option<String>,
    pub(crate) sources: ToolSources,
}

pub(crate) async fn agent_task(
    mut cfg: Config,
    paths: Paths,
    launch: Launch,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    out: Out,
) {
    let Launch { resume, startup_browser, model_override, sources } = launch;
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut active: Option<(Agent, Events)> = None;
    let mut pending = resume.clone();
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
                        a.maybe_learn(tx, false);
                    }
                    continue;
                }
            }
        };
        match command {
            Cmd::Resume(id) => {
                if active.as_ref().is_some_and(|(a, _)| id.as_deref() == Some(a.session_id())) {
                    continue;
                }
                if active.as_ref().is_some_and(|(a, _)| a.jobs().running() > 0) {
                    error(&out, &active, "Cannot switch while a background job is active; wait or cancel it first.");
                    continue;
                }
                pending = id.clone();
                construct_core::llm::overlay::prepare(&mut cfg, &paths, Some(out.landed.clone())).await;
                match prepare(&cfg, &paths, &cwd, id, model_override.as_deref(), &sources) {
                    Ok((a, ready)) => {
                        if let Some((old, _)) = &active {
                            old.finish_learning();
                        }
                        a.set_permission_ask(Arc::new(TuiAsk { tx: out.prompts.clone() }));
                        let tx = session_events(ready.tag.clone(), out.tx.clone());
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
                match prepare(&cfg, &paths, &cwd, pending.clone(), Some(&m), &sources) {
                    Ok((a, ready)) => {
                        if let Some((old, _)) = &active {
                            old.finish_learning();
                        }
                        a.set_permission_ask(Arc::new(TuiAsk { tx: out.prompts.clone() }));
                        let tx = session_events(ready.tag.clone(), out.tx.clone());
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
                    a.maybe_learn(tx, false);
                } else {
                    error(&out, &active, "No active session; use /model to choose an available model, or /resume.");
                    let _ = out.refused.send(());
                }
            }
            Cmd::Skill { name, text, original } => {
                let ran = match &mut active {
                    Some((a, tx)) => {
                        let ran = a.invoke_as(&name, &text, &original, tx).await.unwrap_or(true);
                        a.maybe_learn(tx, false);
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
                    a.maybe_learn(tx, true);
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
                    match a.rename(&title) {
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
                let current_cfg = active.as_ref().map(|(a, _)| a.config()).unwrap_or(&cfg);
                match prepare(current_cfg, &paths, &cwd, None, None, &sources) {
                    Ok((a, ready)) => {
                        if let Some((old, _)) = &active {
                            old.finish_learning();
                        }
                        a.set_permission_ask(Arc::new(TuiAsk { tx: out.prompts.clone() }));
                        let tx = session_events(ready.tag.clone(), out.tx.clone());
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
        a.finish_learning();
    }
}

fn prepare(
    cfg: &Config,
    paths: &Paths,
    cwd: &std::path::Path,
    id: Option<String>,
    model_override: Option<&str>,
    sources: &ToolSources,
) -> anyhow::Result<(Agent, Ready)> {
    let a = Agent::new_with_tools(cfg.clone(), paths.clone(), cwd.to_path_buf(), id, model_override, sources)?;
    construct_core::trace::mark("agent ready");
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
