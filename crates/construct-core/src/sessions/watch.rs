//! Standalone headless watcher loop (`construct watch`, ADR 0079, #416):
//! connects `WakeSources` + `wake.sock` -> `WakeBus` -> `SessionManager`,
//! dispatching wake turns within the concurrency cap and evicting idle agents.

use super::SessionManager;
use crate::config::{Config, Paths};
use crate::event::Event;
use crate::tools::ToolSources;
use crate::wake::{self, SourceCtx, WakeBatch, WakeBus, WakeSources};
use anyhow::Result;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

/// Options for [`run`].
pub struct WatchOptions {
    pub cwd: PathBuf,
    pub no_learn: bool,
}

/// Callback event emitted while `construct watch` runs.
pub enum WatchMsg {
    Event { session: String, event: Box<Event> },
    Status(String),
}

enum Internal {
    Event { session: String, event: Box<Event> },
    TurnFinished { session: String, err: Option<String> },
    JobReady { session: String },
}

/// Run the standalone multi-session headless watcher until `cancel` turns `true`.
pub async fn run(
    cfg: Config,
    paths: Paths,
    tool_sources: ToolSources,
    wake_sources: WakeSources,
    opts: WatchOptions,
    mut cancel: watch::Receiver<bool>,
    mut on_msg: impl FnMut(WatchMsg),
) -> Result<()> {
    let mut bus = WakeBus::start(&cfg.wake, &cfg.wakes, wake_sources.filters().to_vec(), cancel.clone());
    let sock_task = wake::socket::serve(&paths, cfg.wakes.clone(), bus.ingress.clone(), cancel.clone()).await?;
    let source_ctx = SourceCtx { cwd: opts.cwd.clone(), paths: paths.clone(), sandbox: cfg.sandbox.clone() };
    let source_tasks = wake_sources.start_all(&cfg.wakes, &source_ctx, &bus.ingress, &cancel)?;
    on_msg(WatchMsg::Status(format!(
        "watching {} wake trigger(s) on {}",
        cfg.wakes.len(),
        wake::socket::socket_path(&paths).display()
    )));

    let mut mgr = SessionManager::new(cfg, paths, opts.cwd, tool_sources).with_no_learn(opts.no_learn);
    let (int_tx, mut int_rx) = mpsc::unbounded_channel::<Internal>();
    let mut turns: JoinSet<()> = JoinSet::new();
    let mut pending: VecDeque<WakeBatch> = VecDeque::new();
    let mut job_pending: VecDeque<String> = VecDeque::new();

    let evict_period = (mgr.idle_evict_duration() / 2).clamp(Duration::from_millis(50), Duration::from_secs(5));
    let mut evict_tick = tokio::time::interval(evict_period);
    evict_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    break;
                }
            }
            Some(batch) = bus.batches.recv() => {
                pending.push_back(batch);
                dispatch(&mut mgr, &mut pending, &mut job_pending, &mut turns, &int_tx, &mut on_msg);
            }
            Some(msg) = int_rx.recv() => match msg {
                Internal::Event { session, event } => on_msg(WatchMsg::Event { session, event }),
                Internal::TurnFinished { session, err } => {
                    if let Some(e) = err {
                        on_msg(WatchMsg::Status(format!("session {session}: {e}")));
                    }
                    if let Some(jobs) = mgr.finish_turn(&session) {
                        if !jobs.pending().is_empty() {
                            if !job_pending.contains(&session) {
                                job_pending.push_back(session);
                            }
                        } else if jobs.running() > 0 {
                            let tx = int_tx.clone();
                            let sid = session.clone();
                            tokio::spawn(async move {
                                jobs.ready().await;
                                let _ = tx.send(Internal::JobReady { session: sid });
                            });
                        }
                    }
                    dispatch(&mut mgr, &mut pending, &mut job_pending, &mut turns, &int_tx, &mut on_msg);
                }
                Internal::JobReady { session } => {
                    if !job_pending.contains(&session) {
                        job_pending.push_back(session);
                    }
                    dispatch(&mut mgr, &mut pending, &mut job_pending, &mut turns, &int_tx, &mut on_msg);
                }
            },
            _ = evict_tick.tick() => {
                let evicted = mgr.evict_idle();
                for sid in evicted {
                    on_msg(WatchMsg::Status(format!("evicted idle session {sid}")));
                }
            }
            Some(_) = turns.join_next(), if !turns.is_empty() => {}
        }
    }

    mgr.cancel_all();
    while !turns.is_empty() {
        tokio::select! {
            Some(msg) = int_rx.recv() => {
                if let Internal::Event { session, event } = msg {
                    on_msg(WatchMsg::Event { session, event });
                }
            }
            _ = turns.join_next() => {}
        }
    }
    while let Ok(Internal::Event { session, event }) = int_rx.try_recv() {
        on_msg(WatchMsg::Event { session, event });
    }
    mgr.shutdown().await;
    for h in source_tasks {
        h.abort();
    }
    sock_task.abort();
    bus.task.abort();
    Ok(())
}

fn dispatch(
    mgr: &mut SessionManager,
    pending: &mut VecDeque<WakeBatch>,
    job_pending: &mut VecDeque<String>,
    turns: &mut JoinSet<()>,
    int_tx: &mpsc::UnboundedSender<Internal>,
    on_msg: &mut impl FnMut(WatchMsg),
) {
    // First dispatch any job completions on idle sessions.
    let mut deferred_jobs = VecDeque::new();
    while mgr.can_start_turn()
        && let Some(sid) = job_pending.pop_front()
    {
        if mgr.is_busy(&sid) {
            deferred_jobs.push_back(sid);
            continue;
        }
        let Some(agent) = mgr.start_turn(&sid) else {
            continue;
        };
        let tx = int_tx.clone();
        turns.spawn(run_session_wake(agent, sid, Vec::new(), tx));
    }
    job_pending.extend(deferred_jobs);

    // Next dispatch pending wake batches whose target session is idle.
    let mut deferred = VecDeque::new();
    while mgr.can_start_turn()
        && let Some(batch) = pending.pop_front()
    {
        if let Some(existing) = mgr.peek_target(&batch.session)
            && mgr.is_busy(existing)
        {
            deferred.push_back(batch);
            continue;
        }
        let sid = match mgr.resolve(&batch.session, &batch.source_name, &batch.trigger, batch.learn) {
            Ok(id) => id,
            Err(e) => {
                on_msg(WatchMsg::Status(format!("wake {:?} failed to open session: {e:#}", batch.trigger)));
                continue;
            }
        };
        if mgr.is_busy(&sid) {
            deferred.push_back(batch);
            continue;
        }
        let Some(agent) = mgr.start_turn(&sid) else {
            deferred.push_back(batch);
            continue;
        };
        on_msg(WatchMsg::Status(format!(
            "wake {:?} ({} event(s)) -> session {sid}",
            batch.trigger,
            batch.events.len()
        )));
        let tx = int_tx.clone();
        turns.spawn(run_session_wake(agent, sid, batch.events, tx));
    }
    pending.extend(deferred);
}

async fn run_session_wake(
    agent: std::sync::Arc<tokio::sync::Mutex<crate::agent::Agent>>,
    session: String,
    events: Vec<wake::WakeEvent>,
    int_tx: mpsc::UnboundedSender<Internal>,
) {
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    let fwd_tx = int_tx.clone();
    let fwd_sid = session.clone();
    let forward = async move {
        while let Some(event) = ev_rx.recv().await {
            let _ = fwd_tx.send(Internal::Event { session: fwd_sid.clone(), event: Box::new(event) });
        }
    };
    let turn = async move {
        let mut agent = agent.lock().await;
        let res = if events.is_empty() { agent.wake(&ev_tx).await } else { agent.wake_with(events, &ev_tx).await };
        drop(ev_tx);
        res
    };
    let (res, ()) = tokio::join!(turn, forward);
    let err = res.err().map(|e| format!("{e:#}"));
    let _ = int_tx.send(Internal::TurnFinished { session, err });
}
