//! The TUI's `PeerHub` over the hub's control socket (#365, ADR 0074).
//!
//! The TUI never names `construct-hub` (ADR 0069); this maps its three calls
//! onto control-socket requests and the hub's rows onto the TUI's. The design
//! choice: each call opens its own connection, so a hub that starts or stops
//! while the TUI runs is simply found or missed on the next poll.

use construct_hub::Files;
use construct_hub::control::{self, Request, Response};
use construct_hub::registry::State;
use construct_hub::status::PeerStatus;
use construct_tui::{Act, PeerHub, PeerRow, Question, Standing};
use std::path::PathBuf;

pub(in crate::cli) struct Socket {
    path: PathBuf,
}

impl Socket {
    pub(in crate::cli) fn new(paths: &construct_core::config::Paths) -> Self {
        Self { path: Files::new(&paths.home).socket() }
    }

    async fn ask(&self, req: &Request) -> Result<Response, String> {
        match control::request(&self.path, req).await {
            Ok(Some(r)) if r.ok => Ok(r),
            Ok(Some(r)) => Err(r.error.unwrap_or_else(|| "the hub refused".into())),
            Ok(None) => Err("no hub is running for this home".into()),
            Err(e) => Err(format!("{e:#}")),
        }
    }
}

fn row(p: PeerStatus) -> PeerRow {
    PeerRow {
        standing: match p.peer.state {
            State::Expected => Standing::Expected,
            State::Pending => Standing::Pending,
            State::Pinned => Standing::Pinned,
            State::Revoked => Standing::Revoked,
        },
        id: p.peer.id,
        online: p.online,
        working: p.working,
        last_seen: p.peer.last_seen,
        remote: p.remote,
        code: p.code,
        expires: p.expires,
        questions: p
            .questions
            .into_iter()
            .map(|q| {
                let labels = q.labels.into_iter().chain(std::iter::repeat(String::new()));
                Question { title: q.title, options: q.options.into_iter().zip(labels).collect() }
            })
            .collect(),
    }
}

#[async_trait::async_trait]
impl PeerHub for Socket {
    async fn peers(&self) -> Result<Option<Vec<PeerRow>>, String> {
        match control::request(&self.path, &Request::Status).await {
            Ok(Some(r)) => Ok(r.status.map(|s| s.peers.into_iter().map(row).collect())),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    async fn act(&self, act: Act) -> Result<(), String> {
        let req = match act {
            Act::Approve(id) => Request::Approve { id },
            Act::Deny(id) => Request::Deny { id },
            Act::Prompt { id, text } => Request::Prompt { id, text },
            Act::Answer { id, question, title, option } => {
                Request::Answer { id, option, question: Some(question), title: Some(title) }
            }
        };
        self.ask(&req).await.map(|_| ())
    }

    async fn session(&self, id: &str) -> Result<String, String> {
        let r = self.ask(&Request::Session { id: id.to_string() }).await?;
        Ok(r.session.map(|v| v.render()).unwrap_or_default())
    }
}
