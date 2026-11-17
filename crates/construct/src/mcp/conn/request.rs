//! Request lifetimes and serialized, bounded writes on one MCP connection.
//! Cancelling a partial write retires the connection before its stdin lock
//! is released, so another message cannot extend an unfinished JSON line.

use super::{Conn, START_TIMEOUT, State, Stdin, Waiter};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, oneshot};

impl Conn {
    /// Send `method` and wait for its response. The wait is `idle` long, and
    /// starts over on every progress notification for this request. A request
    /// that times out or is dropped (the turn was interrupted) leaves no
    /// pending entry and tells the server with `notifications/cancelled`.
    /// A write gets the same timeout; an incomplete write retires the connection.
    pub async fn request(&self, method: &str, mut params: Value, idle: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = oneshot::channel();
        let progress = Arc::new(Notify::new());
        if !self.state.insert(id, Waiter { tx, progress: progress.clone() }) {
            bail!("MCP server closed");
        }
        let mut guard = Guard { id, method: method.to_string(), state: self.state.clone(), stdin: self.stdin.clone() };
        if method == "tools/call" {
            params["_meta"] = json!({ "progressToken": id });
        }
        write(
            &self.stdin,
            &self.state,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            idle,
        )
        .await
        .with_context(|| format!("writing {method}"))?;
        let v = loop {
            tokio::select! {
                r = &mut rx => break r.map_err(|_| {
                    guard.method.clear(); // nothing to cancel on a dead server
                    anyhow!("MCP server closed")
                })?,
                _ = progress.notified() => continue,
                _ = tokio::time::sleep(idle) => bail!("{method} timed out after {}s with no response or progress", idle.as_secs()),
            }
        };
        guard.method.clear();
        if let Some(e) = v.get("error") {
            bail!("{method}: {}", e["message"].as_str().unwrap_or(&e.to_string()));
        }
        Ok(v["result"].clone())
    }

    pub(super) async fn notify(&self, method: &str) -> Result<()> {
        write(&self.stdin, &self.state, &json!({ "jsonrpc": "2.0", "method": method }), START_TIMEOUT).await
    }
}

/// Undoes a request that did not complete: the pending entry goes, and the
/// server is told to stop (except for `initialize`, which must not be
/// cancelled). An empty `method` means it completed.
struct Guard {
    id: u64,
    method: String,
    state: Arc<State>,
    stdin: Stdin,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.state.take(self.id);
        if self.method.is_empty() || self.method == "initialize" || self.state.lock().dead {
            return;
        }
        let msg = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": self.id, "reason": "timed out or interrupted" } });
        let stdin = self.stdin.clone();
        let state = self.state.clone();
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                let _ = write(&stdin, &state, &msg, START_TIMEOUT).await;
            });
        }
    }
}

/// A write cancelled or failed after taking stdin may have sent half a line.
/// Declared after the stdin guard so it invalidates the stream before unlock.
struct Writing<'a> {
    state: &'a State,
    complete: bool,
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.state.close();
        }
    }
}

pub(super) async fn write(stdin: &Stdin, state: &State, msg: &Value, idle: Duration) -> Result<()> {
    tokio::time::timeout(idle, async {
        let mut w = stdin.lock().await;
        ensure!(!state.lock().dead, "MCP server closed");
        let mut writing = Writing { state, complete: false };
        w.write_all(format!("{msg}\n").as_bytes()).await?;
        w.flush().await?;
        writing.complete = true;
        Ok(())
    })
    .await
    .with_context(|| format!("MCP write timed out after {}s", idle.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::process::Command;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn an_incomplete_write_retires_the_connection_and_fails_pending_calls() {
        for interrupt in [false, true] {
            let mut child = Command::new("sleep").arg("30").stdin(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
            let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
            let state = State::default();
            let (tx, mut rx) = oneshot::channel();
            assert!(state.insert(1, Waiter { tx, progress: Arc::new(Notify::new()) }));
            let msg = json!({ "payload": "x".repeat(1536 * 1024) });
            if interrupt {
                let mut pending = Box::pin(write(&stdin, &state, &msg, Duration::from_secs(30)));
                assert!(futures::poll!(&mut pending).is_pending(), "the server must leave its pipe blocked");
                drop(pending);
            } else {
                let error = write(&stdin, &state, &msg, Duration::from_millis(100)).await.unwrap_err();
                assert!(error.to_string().contains("write timed out"), "{error:#}");
            }
            assert!(state.lock().dead, "an incomplete write left the connection usable");
            assert!(matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Closed)));
            let retry = write(&stdin, &state, &json!({}), Duration::from_secs(1)).await.unwrap_err();
            assert!(retry.to_string().contains("MCP server closed"), "{retry:#}");
            child.kill().await.unwrap();
        }
    }

    #[tokio::test]
    async fn waiting_for_stdin_is_bounded_without_damaging_the_active_writer() {
        let mut child = Command::new("sleep").arg("30").stdin(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let state = State::default();
        let held = stdin.lock().await;
        let error = write(&stdin, &state, &json!({}), Duration::from_millis(100)).await.unwrap_err();
        assert!(error.to_string().contains("write timed out"), "{error:#}");
        assert!(!state.lock().dead, "no bytes were written, so the active writer is still safe");
        drop(held);
        write(&stdin, &state, &json!({}), Duration::from_secs(1)).await.unwrap();
        assert!(!state.lock().dead);
        child.kill().await.unwrap();
    }
}
