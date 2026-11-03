//! Deck's gateway denials: the body its model gateway returns when policy,
//! not the provider, refused a request. The retry loop and the error the agent
//! sees both read this one classification.
//!
//! The design choice: classify by the body's `deck_` code, not by status.
//! `deck_credential_missing` is a 503 that must fail after one attempt, while
//! a real provider 503 still retries; `deck_upstream_failed` is the one Deck
//! code that stays retryable. Anything else falls back to the status text.

use serde_json::Value;
use std::fmt;

pub const BOUNDARY_DENIED: &str = "deck_boundary_denied";
pub const CREDENTIAL_MISSING: &str = "deck_credential_missing";
pub const INVALID_BODY: &str = "deck_invalid_body";
pub const UPSTREAM_FAILED: &str = "deck_upstream_failed";

/// One gateway denial. `code` is Deck's stable string; an unknown `deck_` code
/// is kept so the agent still sees policy rather than a generic status.
#[derive(Debug, Clone)]
pub struct Denial {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub approval_id: Option<String>,
    pub scope: Option<String>,
}

impl Denial {
    /// Parse the response body, the `X-Deck-Approval` header and the status.
    /// `None` when the body is not Deck's JSON shape, so the caller keeps its
    /// status fallback.
    pub fn parse(status: u16, approval: Option<&str>, body: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(body).ok()?;
        let (code, message, inner) = match value.get("error") {
            // The gateway writes `{"error":{"message":…,"type":…,"code":…}}`;
            // ADR 0004 §5 also shows `{"error":"deck_boundary_denied",…}`.
            Some(Value::Object(e)) => (
                e.get("code").and_then(Value::as_str).or_else(|| e.get("type").and_then(Value::as_str))?,
                e.get("message").and_then(Value::as_str).unwrap_or_default(),
                Some(e),
            ),
            Some(Value::String(s)) => {
                (s.as_str(), value.get("message").and_then(Value::as_str).unwrap_or_default(), None)
            }
            _ => return None,
        };
        if !code.starts_with("deck_") {
            return None;
        }
        let field = |k: &str| {
            inner.and_then(|e| e.get(k)).and_then(Value::as_str).or_else(|| value.get(k).and_then(Value::as_str))
        };
        let nonempty = |v: Option<&str>| v.filter(|s| !s.is_empty()).map(str::to_string);
        Some(Self {
            status,
            code: code.to_string(),
            message: message.to_string(),
            approval_id: nonempty(field("approval_id")).or_else(|| nonempty(approval)),
            scope: nonempty(field("scope")),
        })
    }

    /// Whether the same request may be tried again. Only a failed upstream is
    /// a transport problem; every other Deck code is a policy answer.
    pub fn retryable(&self) -> bool {
        self.code == UPSTREAM_FAILED
    }
}

impl fmt::Display for Denial {
    /// Stable first: the code, then the approval and scope the agent needs,
    /// then the gateway's own message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "model endpoint returned {} {}", self.status, self.code)?;
        match (&self.approval_id, &self.scope) {
            (Some(id), Some(scope)) => write!(f, " (approval {id}, scope {scope})")?,
            (Some(id), None) => write!(f, " (approval {id})")?,
            (None, Some(scope)) => write!(f, " (scope {scope})")?,
            (None, None) => {}
        }
        if !self.message.is_empty() {
            write!(f, ": {}", super::truncate(&self.message, 600))?;
        }
        Ok(())
    }
}

impl std::error::Error for Denial {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_gateways_nested_shape() {
        let d = Denial::parse(
            403,
            Some("hdr-1"),
            r#"{"error":{"message":"model \"x\" is not allowed","type":"deck_boundary_denied","code":"deck_boundary_denied","scope":"model:x"}}"#,
        )
        .unwrap();
        assert_eq!((d.code.as_str(), d.approval_id.as_deref()), (BOUNDARY_DENIED, Some("hdr-1")));
        assert_eq!(d.scope.as_deref(), Some("model:x"));
        assert!(d.to_string().contains("approval hdr-1") && d.to_string().contains("scope model:x"));
        assert!(!d.retryable());
    }

    #[test]
    fn the_adr_flat_shape_and_a_body_id_win() {
        let d = Denial::parse(
            403,
            Some("ignored"),
            r#"{"error":"deck_boundary_denied","approval_id":"a1f3","message":"no"}"#,
        )
        .unwrap();
        assert_eq!(d.approval_id.as_deref(), Some("a1f3"));
        assert!(d.to_string().contains("a1f3"));
    }

    #[test]
    fn only_upstream_failed_is_retryable() {
        for (status, code) in
            [(502, UPSTREAM_FAILED), (503, CREDENTIAL_MISSING), (400, INVALID_BODY), (403, BOUNDARY_DENIED)]
        {
            let body = format!(r#"{{"error":{{"message":"m","code":"{code}"}}}}"#);
            let d = Denial::parse(status, None, &body).unwrap();
            assert_eq!(d.retryable(), code == UPSTREAM_FAILED, "{code}");
        }
    }

    #[test]
    fn a_provider_body_is_not_a_deck_denial() {
        assert!(Denial::parse(500, None, r#"{"error":{"message":"boom","type":"server_error"}}"#).is_none());
        assert!(Denial::parse(500, None, "not json").is_none());
    }
}
