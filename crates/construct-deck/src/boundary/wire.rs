//! Boundary wire payloads exchanged with Deck's host gateway and ACP notices.
//!
//! The design choice: `Entry`, `Outcome`, and `Notice` live together as pure
//! validated data above `grant`, so prose formatting (`note`) and the HTTP
//! client (`client`) depend downward on them without cycling.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub approval_id: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default, alias = "outcome")]
    pub state: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default, deserialize_with = "super::grant::expires_at_ms")]
    pub expires_at_ms: Option<u64>,
}

impl Entry {
    pub(super) fn validate(self) -> Result<Self, String> {
        super::grant::timed_has_deadline(self.scope.as_deref(), self.expires_at_ms)?;
        Ok(self)
    }
    pub fn state(&self) -> &str {
        self.state.as_deref().unwrap_or("pending")
    }
    /// A decision, as opposed to an item still waiting on the user.
    pub fn terminal(&self) -> bool {
        matches!(self.state(), "allowed" | "denied" | "expired" | "withdrawn")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Outcome {
    #[serde(default, alias = "state")]
    pub outcome: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default, deserialize_with = "super::grant::expires_at_ms")]
    pub expires_at_ms: Option<u64>,
}

impl Outcome {
    pub(super) fn validate(self) -> Result<Self, String> {
        super::grant::timed_has_deadline(self.scope.as_deref(), self.expires_at_ms)?;
        Ok(self)
    }
    pub fn terminal(&self) -> bool {
        matches!(self.outcome.as_str(), "allowed" | "denied" | "expired" | "withdrawn")
    }
}

/// A decision Deck pushed over ACP (either boundary method). It informs; it
/// grants nothing and the agent cannot answer it with a decision.
#[derive(Debug, Clone, Deserialize)]
pub struct Notice {
    #[serde(rename = "approval_id", alias = "id", deserialize_with = "approval_id")]
    pub id: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default, alias = "state")]
    pub outcome: Option<String>,
    #[serde(default, deserialize_with = "super::grant::optional_string")]
    pub scope: Option<String>,
    #[serde(default, deserialize_with = "super::grant::expires_at_ms")]
    pub expires_at_ms: Option<u64>,
    #[serde(default, rename = "sessionId", alias = "session_id")]
    pub session: Option<String>,
}

/// Empty IDs cannot identify a decision for deduplication (contract §5.3).
fn approval_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let id = String::deserialize(deserializer)?;
    if id.is_empty() {
        return Err(serde::de::Error::custom("approval id must not be empty"));
    }
    Ok(id)
}

impl Notice {
    pub fn validate(self) -> Result<Self, String> {
        super::grant::timed_has_deadline(self.scope.as_deref(), self.expires_at_ms)?;
        Ok(self)
    }
    pub fn outcome(&self) -> &str {
        self.outcome.as_deref().unwrap_or("decided")
    }
}
