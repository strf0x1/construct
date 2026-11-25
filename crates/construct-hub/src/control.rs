//! The hub's control socket: newline-delimited JSON over a `0600` unix socket
//! inside `$CONSTRUCT_HOME` (#360).
//!
//! Any number of clients (TUIs, `construct hub status`, later `construct
//! build` and the `peer` tool) attach at once; each line is one request and
//! gets one response line. The socket is made `0600` right after bind; the
//! moment between is covered by the home's `0700` (ADR 0021), which keeps
//! other users from reaching the path at all. (Narrowing the umask instead
//! would be process-wide and race the hub's other threads.)

use crate::invite::InviteReply;
use crate::status::Status;
use crate::view::SessionView;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Longest request line a client may send.
pub const MAX_LINE: u64 = 64 * 1024;

/// What a client asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// The hub and its peers.
    Status,
    /// Close every peer link and exit.
    Stop,
    /// Open an `expected` slot for `id` with a fresh single-use join token.
    Invite {
        id: String,
        ttl_secs: u64,
        /// The URL to print in the command (default: the hub's own).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Pin the key of the join waiting under `id`.
    Approve { id: String },
    /// Turn away the join waiting under `id`.
    Deny { id: String },
    /// Revoke `id`: its key is refused from now on and a live link is told.
    Revoke { id: String },
    /// Let unknown ids wait for approval for `secs` (0 closes the window).
    Pair { secs: u64 },
    /// Send `text` to pinned peer `id`'s session (#362); answered at once,
    /// while the turn runs. Sent when the peer attaches if it is away.
    Prompt { id: String, text: String },
    /// Cancel the turn running on `id`.
    Cancel { id: String },
    /// `id`'s session as the hub holds it: turn, transcript, questions.
    Session { id: String },
    /// Answer `id`'s open permission question `question` (1-based; may be
    /// left out when only one is open) with option `option`.
    Answer {
        id: String,
        option: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<usize>,
    },
}

/// The answer to one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Box<Status>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<InviteReply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Box<SessionView>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    #[must_use]
    pub fn ok() -> Self {
        Self { ok: true, status: None, invite: None, session: None, error: None }
    }

    #[must_use]
    pub fn status(s: Status) -> Self {
        Self { status: Some(Box::new(s)), ..Self::ok() }
    }

    #[must_use]
    pub fn session(v: SessionView) -> Self {
        Self { session: Some(Box::new(v)), ..Self::ok() }
    }

    #[must_use]
    pub fn invite(i: InviteReply) -> Self {
        Self { invite: Some(i), ..Self::ok() }
    }

    /// `ok` for `Ok(())`, the error's text otherwise.
    #[must_use]
    pub fn of(r: anyhow::Result<()>) -> Self {
        r.map_or_else(|e| Self::error(format!("{e:#}")), |()| Self::ok())
    }

    #[must_use]
    pub fn error(e: impl Into<String>) -> Self {
        Self { ok: false, error: Some(e.into()), ..Self::ok() }
    }
}

/// Send one request to the hub at `socket`. `Ok(None)` means no hub is
/// listening there (no socket, or a stale one nobody accepts on).
pub async fn request(socket: &Path, req: &Request) -> Result<Option<Response>> {
    let stream = match UnixStream::connect(socket).await {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => {
            return Ok(None);
        }
        Err(e) => return Err(e).with_context(|| format!("connect {}", socket.display())),
    };
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    let mut reply = String::new();
    BufReader::new(read.take(MAX_LINE * 16)).read_line(&mut reply).await?;
    if reply.is_empty() {
        anyhow::bail!("the hub closed the control socket without answering");
    }
    Ok(Some(serde_json::from_str(&reply).context("parse hub response")?))
}

/// Bind the control socket at `path`, replacing a stale socket file (the
/// caller holds the instance lock, so no live hub owns it).
pub fn bind(path: &Path) -> Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("remove stale {}", path.display())),
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))?;
    Ok(listener)
}

/// Answer requests on `listener` with `handle` until the task is dropped.
pub async fn serve<H>(listener: UnixListener, handle: H)
where
    H: Fn(Request) -> Response + Clone + Send + Sync + 'static,
{
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let handle = handle.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read.take(MAX_LINE * 1024));
            let mut line = String::new();
            loop {
                line.clear();
                match lines.read_line(&mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) if n as u64 > MAX_LINE => return,
                    Ok(_) => {}
                }
                let resp = match serde_json::from_str::<Request>(line.trim()) {
                    Ok(req) => handle(req),
                    Err(e) => Response::error(format!("bad request: {e}")),
                };
                let Ok(mut out) = serde_json::to_vec(&resp) else { return };
                out.push(b'\n');
                if write.write_all(&out).await.is_err() {
                    return;
                }
            }
        });
    }
}
