//! The `peer` tool's `Hub` over the hub's control socket (#366, ADR 0075).
//!
//! The tool crate never names `construct-hub`; this maps its four calls onto
//! control-socket requests and the hub's session view onto the tool's. The
//! design choice: like the TUI's (`tui.rs`), each call opens its own
//! connection, so a hub that restarts mid-watch is simply found again.

use construct_hub::Files;
use construct_hub::control::{self, Request, Response};
use construct_hub::view::SessionView;
use construct_tools_peers::{Answered, Hub, PeerRow, Session};
use std::path::PathBuf;

pub(in crate::cli) struct Socket {
    files: Files,
}

impl Socket {
    pub(in crate::cli) fn new(paths: &construct_core::config::Paths) -> Self {
        Self { files: Files::new(&paths.home) }
    }

    fn path(&self) -> PathBuf {
        self.files.socket()
    }

    async fn ask(&self, req: &Request) -> Result<Response, String> {
        match control::request(&self.path(), req).await {
            Ok(Some(r)) if r.ok => Ok(r),
            Ok(Some(r)) => Err(r.error.unwrap_or_else(|| "the hub refused".into())),
            Ok(None) => Err("no hub is running on this host; the user starts one with `construct hub`".into()),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    async fn view(&self, req: &Request) -> Result<SessionView, String> {
        self.ask(req).await?.session.map(|v| *v).ok_or_else(|| "the hub sent no session".to_string())
    }
}

fn session(v: SessionView) -> Session {
    let state = v.turn.as_ref().and_then(|t| t["state"].as_str()).unwrap_or("idle");
    Session {
        attached: v.attached,
        working: v.key.is_some() || state != "idle",
        turn: v.turn.as_ref().and_then(|t| t["turn"].as_u64()),
        key: v.key,
        answered: v.answered.map(|a| Answered { key: a.key, stop: a.stop, turn: a.turn, reply: a.reply }),
        questions: v.questions.into_iter().map(|q| q.title).collect(),
        transcript: v.transcript,
        error: v.error,
    }
}

#[async_trait::async_trait]
impl Hub for Socket {
    fn present(&self) -> bool {
        self.files.key().exists()
    }

    async fn peers(&self) -> Result<Vec<PeerRow>, String> {
        let status = self.ask(&Request::Status).await?.status.ok_or("the hub sent no status")?;
        Ok(status
            .peers
            .into_iter()
            .map(|p| PeerRow {
                state: serde_json::to_value(p.peer.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                id: p.peer.id,
                online: p.online,
                working: p.working,
                questions: p.questions.len(),
            })
            .collect())
    }

    async fn send(&self, peer: &str, text: &str) -> Result<String, String> {
        let v = self.view(&Request::Prompt { id: peer.to_string(), text: text.to_string() }).await?;
        v.key.ok_or_else(|| format!("the hub took the prompt for {peer} but shows none in flight"))
    }

    async fn session(&self, peer: &str) -> Result<Session, String> {
        self.view(&Request::Session { id: peer.to_string() }).await.map(session)
    }

    async fn cancel(&self, peer: &str) -> Result<(), String> {
        self.ask(&Request::Cancel { id: peer.to_string() }).await.map(|_| ())
    }
}
