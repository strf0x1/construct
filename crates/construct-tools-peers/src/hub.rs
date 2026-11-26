//! What the `peer` tool needs from the hub, as plain data and one trait the
//! binary implements over the hub's control socket.
//!
//! The design choice: these types are the tool's own, not the hub's, so this
//! crate links nothing of `construct-hub` (ADR 0074, 0075).

/// One peer as the hub's registry and live link show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRow {
    pub id: String,
    /// `expected`, `pending`, `pinned` or `revoked`.
    pub state: String,
    pub online: bool,
    pub working: bool,
    /// Permission questions waiting for a human at the hub.
    pub questions: usize,
}

/// A prompt the peer answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    /// The idempotency key it was sent with.
    pub key: String,
    /// `end_turn`, `cancelled`, `error: …`.
    pub stop: String,
    pub turn: Option<u64>,
    /// The peer agent's words in reply.
    pub reply: String,
}

/// One peer's session as the hub holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub attached: bool,
    pub working: bool,
    pub turn: Option<u64>,
    /// The key of the prompt in flight.
    pub key: Option<String>,
    pub answered: Option<Answered>,
    /// Titles of the permission questions waiting for a human.
    pub questions: Vec<String>,
    pub transcript: String,
    /// Why the session could not be opened.
    pub error: Option<String>,
}

/// What the tool needs from the hub. Errors are prose for the model.
#[async_trait::async_trait]
pub trait Hub: Send + Sync {
    /// Whether this home has a hub at all (its key exists). Asked once per
    /// catalog, so it must not wait on anything.
    fn present(&self) -> bool;
    async fn peers(&self) -> Result<Vec<PeerRow>, String>;
    /// Send `text` to `peer`'s session; the prompt's idempotency key.
    async fn send(&self, peer: &str, text: &str) -> Result<String, String>;
    async fn session(&self, peer: &str) -> Result<Session, String>;
    /// Cancel the turn running on `peer`.
    async fn cancel(&self, peer: &str) -> Result<(), String>;
}
