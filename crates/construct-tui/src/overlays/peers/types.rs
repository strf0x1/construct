//! Wire types and the [`PeerHub`] trait shared across the peer tile, popup,
//! attach view and poller. Pure data and sanitizers with no UI state.

use std::time::{SystemTime, UNIX_EPOCH};

/// How the user closed the peer popup, so `motion` can tint its ghost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow { pop: bool },
    Always { pop: bool },
    Deny { pop: bool },
    Later,
}

/// Where a peer stands in the hub's registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// A slot an invite or `construct build` made; nobody has joined it yet.
    Expected,
    /// A join waiting for a human, with its link open.
    Pending,
    Pinned,
    Revoked,
}

/// A permission question a peer is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub title: String,
    /// `(option id, the peer's wording)`, in the order offered.
    pub options: Vec<(String, String)>,
}

/// One peer as the hub reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRow {
    pub id: String,
    pub standing: Standing,
    pub online: bool,
    /// A prompt is in flight or the peer's turn is running.
    pub working: bool,
    /// Unix seconds of the last message or disconnect.
    pub last_seen: Option<u64>,
    /// The address the live link came from.
    pub remote: Option<String>,
    /// The SAS code of a waiting join; the peer prints the same.
    pub code: Option<String>,
    /// Unix seconds when a waiting join is turned away.
    pub expires: Option<u64>,
    pub questions: Vec<Question>,
    /// The model its calls use, and whether the hub set it (#390).
    pub model: Option<(String, bool)>,
}

/// What a human did about a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Approve(String),
    Deny(String),
    Prompt {
        id: String,
        text: String,
    },
    /// Use `model` for a peer's model calls; `None` clears the override.
    Model {
        id: String,
        model: Option<String>,
    },
    /// Answer question `question` (1-based) with `option`; `title` is the
    /// question the human saw, so a list that moved under them is refused.
    Answer {
        id: String,
        question: usize,
        title: String,
        option: String,
    },
}

/// The hub, as the TUI reaches it. The binary implements it over the hub's
/// control socket when it is built with the `hub` feature.
#[async_trait::async_trait]
pub trait PeerHub: Send + Sync {
    /// Every peer; `Ok(None)` when no hub runs for this home.
    async fn peers(&self) -> Result<Option<Vec<PeerRow>>, String>;
    async fn act(&self, act: Act) -> Result<(), String>;
    /// One peer's session as text (transcript, turn, questions).
    async fn session(&self, id: &str) -> Result<String, String>;
}

/// What the poller tells the UI.
pub(crate) enum Msg {
    Peers(Option<Vec<PeerRow>>),
    /// An act finished: what to say about it, or why it failed.
    Done(Result<String, String>),
    Session(String, Result<String, String>),
}

/// Peer text cannot emit terminal commands or hide content with carriage returns.
pub(super) fn clean(text: &str) -> String {
    text.chars()
        .flat_map(|c| if c.is_control() && c != '\n' { c.escape_default().collect() } else { vec![c] })
        .collect()
}

/// Unix seconds now.
pub(super) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
pub(super) fn test_row(id: &str, standing: Standing) -> PeerRow {
    PeerRow {
        id: id.into(),
        standing,
        online: true,
        working: false,
        last_seen: None,
        remote: Some("10.0.0.5:5000".into()),
        code: None,
        expires: None,
        questions: vec![],
        model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_text_cannot_carry_escapes() {
        assert_eq!(clean("a\x1b[2Jb\rc\nd"), "a\\u{1b}[2Jb\\rc\nd");
    }
}
