//! Local `0600` JSON-lines Unix socket (`$CONSTRUCT_HOME/wake.sock`) for
//! `construct wake emit <name> [--json <payload>]` and `construct wake list`
//! (ADR 0079, #416). Matches `hub.sock`'s framing and permissions.

use super::{SessionPolicy, WakeEvent, WakeTrust, next_id, now_ms};
use crate::config::{Paths, WakeTriggerConfig};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// The longest socket path every supported platform accepts (macOS's
/// `sun_path` is 104 bytes including the trailing NUL), as for `hub.sock`.
const MAX_SOCKET_PATH: usize = 103;

/// Path to the local wake control socket inside `$CONSTRUCT_HOME`.
pub fn socket_path(paths: &Paths) -> PathBuf {
    paths.home.join("wake.sock")
}

fn check_len(path: &Path) -> Result<()> {
    let n = path.as_os_str().len();
    if n > MAX_SOCKET_PATH {
        bail!(
            "{} is {n} bytes, longer than a Unix socket path may be ({MAX_SOCKET_PATH}); use a shorter CONSTRUCT_HOME",
            path.display()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EmitRequest {
    Emit {
        name: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        data: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dedup_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untrusted: Option<bool>,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EmitReply {
    Ok { id: String, trigger: String },
    Pong { triggers: Vec<String> },
    Error { message: String },
}

/// Bind `$CONSTRUCT_HOME/wake.sock` (mode `0600`) and forward emitted events
/// into `ingress` until `cancel` turns `true`.
pub async fn serve(
    paths: &Paths,
    triggers: Vec<WakeTriggerConfig>,
    ingress: mpsc::UnboundedSender<WakeEvent>,
    mut cancel: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let path = socket_path(paths);
    check_len(&path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        // If another watcher is already alive on this socket, do not steal it.
        if UnixStream::connect(&path).await.is_ok() {
            bail!("another watcher is already listening on {}", path.display());
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let cleanup_path = path.clone();
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    let triggers = triggers.clone();
                    let ingress = ingress.clone();
                    tokio::spawn(async move {
                        let _ = handle_conn(stream, &triggers, &ingress).await;
                    });
                }
            }
        }
        let _ = std::fs::remove_file(&cleanup_path);
    });
    Ok(handle)
}

async fn handle_conn(
    stream: UnixStream,
    triggers: &[WakeTriggerConfig],
    ingress: &mpsc::UnboundedSender<WakeEvent>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let reply = match serde_json::from_str::<EmitRequest>(line.trim()) {
        Ok(EmitRequest::Ping) => EmitReply::Pong { triggers: triggers.iter().map(|t| t.name.clone()).collect() },
        Ok(EmitRequest::Emit { name, summary, data, dedup_key, untrusted }) => {
            match triggers.iter().find(|t| t.name == name) {
                Some(cfg) => {
                    let id = next_id();
                    let summary =
                        if summary.trim().is_empty() { format!("CLI wake emitted for {name}") } else { summary };
                    let event = WakeEvent {
                        id: id.clone(),
                        source_kind: "cli".into(),
                        source_name: cfg.name.clone(),
                        dedup_key,
                        occurred_at: now_ms(),
                        trust: WakeTrust::from_untrusted(untrusted.unwrap_or_else(|| cfg.is_untrusted())),
                        summary,
                        data,
                        trigger: cfg.name.clone(),
                        session: SessionPolicy::parse(&cfg.session, &cfg.name),
                        prompt: cfg.prompt.clone(),
                        learn: cfg.learn,
                        notes: Vec::new(),
                    };
                    if ingress.send(event).is_ok() {
                        EmitReply::Ok { id, trigger: cfg.name.clone() }
                    } else {
                        EmitReply::Error { message: "wake bus is shutting down".into() }
                    }
                }
                None => EmitReply::Error {
                    message: format!("unknown wake trigger {name:?} (configured: {})", names_list(triggers)),
                },
            }
        }
        Err(e) => EmitReply::Error { message: format!("bad wake request: {e}") },
    };
    let mut out = serde_json::to_string(&reply)?;
    out.push('\n');
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

fn names_list(triggers: &[WakeTriggerConfig]) -> String {
    if triggers.is_empty() {
        "none".into()
    } else {
        triggers.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// Send one request to `$CONSTRUCT_HOME/wake.sock`.
pub async fn send(paths: &Paths, req: &EmitRequest) -> Result<EmitReply> {
    send_to(&socket_path(paths), req).await
}

async fn send_to(path: &Path, req: &EmitRequest) -> Result<EmitReply> {
    check_len(path)?;
    let mut stream = UnixStream::connect(path).await.with_context(|| {
        format!("connecting to {} (is `construct watch` or `construct peer` running?)", path.display())
    })?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).await?;
    serde_json::from_str(resp.trim()).context("reading wake socket reply")
}

/// Whether a local watcher is currently listening on `wake.sock`.
pub async fn is_listening(paths: &Paths) -> bool {
    matches!(send(paths, &EmitRequest::Ping).await, Ok(EmitReply::Pong { .. }))
}
