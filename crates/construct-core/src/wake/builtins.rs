//! Built-in zero-dependency wake sources: `timer`, sandboxed `command` probe,
//! and `cli` (events emitted via `wake.sock`) (ADR 0079, #416).
//!
//! The design choice: `command` probes always run through `sandbox::Policy`
//! (the exact same helper and confinement as the shell tool) and stay quiet
//! when exit == 0 and output is empty so routine healthy probes spend zero
//! tokens.

use super::{SessionPolicy, SourceCtx, WakeEvent, WakeSource, WakeTrust, next_id, now_ms};
use crate::config::WakeTriggerConfig;
use crate::sandbox::Policy;
use anyhow::Result;
use serde_json::json;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

const PROBE_OUTPUT_MAX: usize = 8 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Periodic wall-clock interval wake producer (`kind = "timer"`).
pub struct TimerSource;

impl WakeSource for TimerSource {
    fn kinds(&self) -> &[&str] {
        &["timer"]
    }

    fn start(
        &self,
        triggers: &[WakeTriggerConfig],
        _ctx: &SourceCtx,
        tx: mpsc::UnboundedSender<WakeEvent>,
        cancel: watch::Receiver<bool>,
    ) -> Result<Vec<JoinHandle<()>>> {
        let mut handles = Vec::new();
        for cfg in triggers {
            let every = cfg
                .every_duration()
                .map_err(anyhow::Error::msg)?
                .filter(|d| !d.is_zero())
                .ok_or_else(|| anyhow::anyhow!("timer {:?} needs non-zero `every`", cfg.name))?;
            let cfg = cfg.clone();
            let tx = tx.clone();
            let mut cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval_at(Instant::now() + every, every);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                let mut tick = 0u64;
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.changed() => {
                            if *cancel.borrow() {
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            tick += 1;
                            let ev = WakeEvent {
                                id: next_id(),
                                source_kind: "timer".into(),
                                source_name: cfg.name.clone(),
                                dedup_key: None,
                                occurred_at: now_ms(),
                                trust: WakeTrust::from_untrusted(cfg.is_untrusted()),
                                summary: format!("Timer {:?} tick #{tick} (every {})", cfg.name, cfg.every.as_deref().unwrap_or("")),
                                data: json!({ "tick": tick, "every": cfg.every }),
                                trigger: cfg.name.clone(),
                                session: SessionPolicy::parse(&cfg.session, &cfg.name),
                                prompt: cfg.prompt.clone(),
                                learn: cfg.learn,
                            };
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                    }
                }
            }));
        }
        Ok(handles)
    }
}

/// Periodic sandboxed shell probe producer (`kind = "command"`). Emits a wake
/// only when the command exits non-zero or writes non-empty stdout/stderr.
pub struct CommandSource;

impl WakeSource for CommandSource {
    fn kinds(&self) -> &[&str] {
        &["command"]
    }

    fn start(
        &self,
        triggers: &[WakeTriggerConfig],
        ctx: &SourceCtx,
        tx: mpsc::UnboundedSender<WakeEvent>,
        cancel: watch::Receiver<bool>,
    ) -> Result<Vec<JoinHandle<()>>> {
        let mut handles = Vec::new();
        for cfg in triggers {
            let every = cfg
                .every_duration()
                .map_err(anyhow::Error::msg)?
                .filter(|d| !d.is_zero())
                .ok_or_else(|| anyhow::anyhow!("command {:?} needs non-zero `every`", cfg.name))?;
            let script = cfg
                .command
                .clone()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("command {:?} needs `command`", cfg.name))?;
            let cfg = cfg.clone();
            let ctx = ctx.clone();
            let tx = tx.clone();
            let mut cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval_at(Instant::now() + every, every);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.changed() => {
                            if *cancel.borrow() {
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            if let Some(ev) = run_probe(&cfg, &script, &ctx).await
                                && tx.send(ev).is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            }));
        }
        Ok(handles)
    }
}

async fn run_probe(cfg: &WakeTriggerConfig, script: &str, ctx: &SourceCtx) -> Option<WakeEvent> {
    let policy = Policy::new(&ctx.sandbox, &ctx.cwd, &ctx.paths);
    let mut cmd = policy.command("/bin/sh", script);
    cmd.current_dir(&ctx.cwd);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let output = match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Some(WakeEvent {
                id: next_id(),
                source_kind: "command".into(),
                source_name: cfg.name.clone(),
                dedup_key: None,
                occurred_at: now_ms(),
                trust: WakeTrust::from_untrusted(cfg.is_untrusted()),
                summary: format!("Probe {:?} failed to launch: {e}", cfg.name),
                data: json!({ "error": e.to_string() }),
                trigger: cfg.name.clone(),
                session: SessionPolicy::parse(&cfg.session, &cfg.name),
                prompt: cfg.prompt.clone(),
                learn: cfg.learn,
            });
        }
        Err(_) => {
            return Some(WakeEvent {
                id: next_id(),
                source_kind: "command".into(),
                source_name: cfg.name.clone(),
                dedup_key: None,
                occurred_at: now_ms(),
                trust: WakeTrust::from_untrusted(cfg.is_untrusted()),
                summary: format!("Probe {:?} timed out after {}s", cfg.name, PROBE_TIMEOUT.as_secs()),
                data: json!({ "timeout": true }),
                trigger: cfg.name.clone(),
                session: SessionPolicy::parse(&cfg.session, &cfg.name),
                prompt: cfg.prompt.clone(),
                learn: cfg.learn,
            });
        }
    };
    let exit = output.status.code().unwrap_or(-1);
    let stdout = crate::text::tail(&String::from_utf8_lossy(&output.stdout), PROBE_OUTPUT_MAX).to_string();
    let stderr = crate::text::tail(&String::from_utf8_lossy(&output.stderr), PROBE_OUTPUT_MAX).to_string();
    if exit == 0 && stdout.trim().is_empty() && stderr.trim().is_empty() {
        return None;
    }
    let first_line =
        stdout.lines().chain(stderr.lines()).map(str::trim).find(|l| !l.is_empty()).unwrap_or("non-zero exit");
    let summary = format!("Probe {:?} (exit {exit}): {}", cfg.name, crate::text::truncate(first_line, 200));
    Some(WakeEvent {
        id: next_id(),
        source_kind: "command".into(),
        source_name: cfg.name.clone(),
        dedup_key: None,
        occurred_at: now_ms(),
        trust: WakeTrust::from_untrusted(cfg.is_untrusted()),
        summary,
        data: json!({ "exit": exit, "stdout": stdout, "stderr": stderr }),
        trigger: cfg.name.clone(),
        session: SessionPolicy::parse(&cfg.session, &cfg.name),
        prompt: cfg.prompt.clone(),
        learn: cfg.learn,
    })
}

/// Marker source for `kind = "cli"` triggers emitted via `construct wake emit`
/// over `$CONSTRUCT_HOME/wake.sock`.
pub struct CliSource;

impl WakeSource for CliSource {
    fn kinds(&self) -> &[&str] {
        &["cli"]
    }

    fn start(
        &self,
        _triggers: &[WakeTriggerConfig],
        _ctx: &SourceCtx,
        _tx: mpsc::UnboundedSender<WakeEvent>,
        _cancel: watch::Receiver<bool>,
    ) -> Result<Vec<JoinHandle<()>>> {
        Ok(Vec::new())
    }
}
