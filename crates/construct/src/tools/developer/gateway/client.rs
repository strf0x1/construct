//! The HTTP side of Deck's GitHub operation API (contract 1.2.0 §11): one
//! POST per operation to the gateway named by `DECK_GITHUB_URL`, and every
//! refusal turned into prose that says what the model should do next.
//!
//! The design choice: the refusal's `code` and `retryable`, never the status
//! alone, decide the wording. A rewrite or a moved branch names the commit to
//! rebase onto; a pending approval names its id and the boundary tool to wait
//! with; anything not retryable says so, so the model neither repeats it
//! unchanged nor looks for another route to GitHub.

use crate::tools::ToolError;
use crate::tools::boundary::WAIT;
use serde_json::{Value, json};
use std::time::Duration;

/// The gateway's paths, one per operation (contract 1.2.0 §11.2).
pub const PUBLISH: &str = "/operations/publish";
pub const REFS: &str = "/operations/refs";
pub const CREATE_PR: &str = "/operations/pull_request";
pub const UPDATE_PR: &str = "/operations/pull_request/update";
pub const REVIEW: &str = "/operations/pull_request/review";

/// The largest bundle the gateway takes; a larger one is refused here rather
/// than streamed to a 413.
pub const MAX_BUNDLE: usize = 16 * 1024 * 1024;

/// A publish carries git objects and waits for the host's push.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(180);
const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Gateway {
    base: String,
    http: reqwest::Client,
}

impl Gateway {
    pub fn new(base: &str) -> Result<Self, ToolError> {
        // Loopback in the guest: a proxy variable must not send it elsewhere.
        let http = reqwest::Client::builder()
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(2))
            .no_proxy()
            .build()
            .map_err(|_| ToolError::new("gateway", "could not start the client for Deck's GitHub gateway"))?;
        Ok(Self { base: base.trim_end_matches('/').to_string(), http })
    }

    /// One JSON operation.
    pub async fn call(&self, path: &str, body: &Value) -> Result<Value, ToolError> {
        self.send(path, body.to_string().into_bytes(), TIMEOUT).await
    }

    /// `publish`: the JSON head, a 4-byte big-endian length, then the bundle,
    /// all in one body (contract 1.2.0 §11.3).
    pub async fn publish(&self, head: &Value, bundle: &[u8]) -> Result<Value, ToolError> {
        self.send(PUBLISH, frame(head, bundle)?, PUBLISH_TIMEOUT).await
    }

    async fn send(&self, path: &str, body: Vec<u8>, timeout: Duration) -> Result<Value, ToolError> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| {
                let what = if error.is_timeout() { "timed out" } else { "could not be reached" };
                ToolError::new(
                    "unknown",
                    format!("Deck's GitHub gateway {what}. The outcome is unknown: read the branch or pull request back (pr_review, or push again to see the branch's state) before repeating a write."),
                )
            })?;
        let status = response.status().as_u16();
        let approval = response.headers().get("x-deck-approval").and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = response.text().await.map_err(|_| {
            ToolError::new("unknown", "Deck's GitHub gateway reply was lost; reconcile before repeating a write")
        })?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status == 200 {
            return Ok(value);
        }
        Err(refusal(status, &value, approval.as_deref()))
    }
}

/// The publish body.
pub fn frame(head: &Value, bundle: &[u8]) -> Result<Vec<u8>, ToolError> {
    if bundle.len() > MAX_BUNDLE {
        return Err(ToolError::new(
            "too_large",
            format!("the bundle is {} bytes; Deck's gateway takes at most {MAX_BUNDLE}", bundle.len()),
        ));
    }
    let length = u32::try_from(bundle.len()).map_err(|_| "the bundle does not fit a 32-bit length")?;
    let mut body = head.to_string().into_bytes();
    body.extend_from_slice(&length.to_be_bytes());
    body.extend_from_slice(bundle);
    Ok(body)
}

/// A refusal as the model reads it (contract 1.2.0 §11.5).
pub fn refusal(status: u16, body: &Value, header: Option<&str>) -> ToolError {
    let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or_default();
    let message = body.pointer("/error/message").and_then(Value::as_str).unwrap_or("no reason given");
    let retryable = body.get("retryable").and_then(Value::as_bool).unwrap_or(false);
    let current = body.get("current").and_then(Value::as_str);
    let approval = body.get("approval_id").and_then(Value::as_str).or(header);
    let data =
        json!({ "status": status, "code": code, "retryable": retryable, "current": current, "approval": approval });
    let next = match (code, current, approval) {
        ("deck_github_refused" | "deck_github_conflict", Some(sha), _) => {
            format!("The branch is at {sha}: rebase onto {sha} and push again. Do not resend the same commits.")
        }
        ("deck_boundary_denied", _, Some(id)) if retryable => format!(
            "Approval {id} is pending with the user. Call {WAIT} with id {id} if the task needs it, then retry the same operation. Do not look for another route to GitHub."
        ),
        ("deck_boundary_denied", _, _) => {
            "The user's boundary policy denied it. Do not retry it or look for another route to GitHub.".into()
        }
        ("deck_not_permitted", _, _) => {
            "A Deck task may push its branch, open or update its pull request, and read it; nothing else.".into()
        }
        (
            "deck_github_credential"
            | "deck_credential_missing"
            | "deck_credential_refused"
            | "deck_credential_revoked",
            _,
            _,
        ) => "This is the host's credential, which only the user can fix. Report it; do not retry.".into(),
        _ if retryable => "It may succeed later; retry once the cause has passed.".into(),
        _ if code.is_empty() => "The reply was not Deck's refusal shape; treat the outcome as unknown.".into(),
        _ => "Do not retry it unchanged.".into(),
    };
    let label = if code.is_empty() { format!("HTTP {status}") } else { format!("{code} ({status})") };
    ToolError::new("gateway", format!("Deck's GitHub gateway refused: {label}: {message}. {next}")).with_data(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(code: &str, retryable: bool, extra: Value) -> Value {
        let mut b = json!({ "error": { "message": "why", "type": code, "code": code }, "retryable": retryable });
        if let (Some(b), Some(extra)) = (b.as_object_mut(), extra.as_object()) {
            b.extend(extra.clone());
        }
        b
    }

    #[test]
    fn a_rewrite_or_a_moved_branch_names_the_commit_to_rebase_onto() {
        let sha = "a".repeat(40);
        for code in ["deck_github_refused", "deck_github_conflict"] {
            let e = refusal(403, &body(code, code.ends_with("conflict"), json!({ "current": sha })), None);
            assert!(e.message.contains(&format!("rebase onto {sha} and push again")), "{}", e.message);
            assert_eq!(e.data.unwrap()["current"], json!(sha));
        }
    }

    #[test]
    fn a_pending_approval_names_its_id_and_the_wait_tool() {
        let e = refusal(403, &body("deck_boundary_denied", true, json!({ "approval_id": "g1" })), None);
        assert!(e.message.contains("Approval g1 is pending") && e.message.contains(WAIT), "{}", e.message);
        let header = refusal(403, &body("deck_boundary_denied", true, json!({})), Some("g2"));
        assert!(header.message.contains("g2"), "the header carries the id too: {}", header.message);
        let denied = refusal(403, &body("deck_boundary_denied", false, json!({ "approval_id": "g3" })), None);
        assert!(denied.message.contains("denied") && !denied.message.contains("pending"), "{}", denied.message);
    }

    #[test]
    fn the_rest_say_whether_to_retry() {
        let e = refusal(403, &body("deck_not_permitted", false, json!({})), None);
        assert!(e.message.contains("nothing else"), "{}", e.message);
        let e = refusal(502, &body("deck_github_failed", true, json!({})), None);
        assert!(e.message.contains("retry once"), "{}", e.message);
        let e = refusal(403, &body("deck_credential_revoked", false, json!({})), None);
        assert!(e.message.contains("only the user can fix"), "{}", e.message);
        let e = refusal(500, &json!("not json"), None);
        assert!(e.message.contains("HTTP 500") && e.message.contains("unknown"), "{}", e.message);
    }

    #[test]
    fn a_publish_body_is_the_head_a_length_and_the_bundle() {
        let head = json!({ "operation": "publish", "branch": "deck/t-1" });
        let body = frame(&head, b"BUNDLE").unwrap();
        let json = head.to_string();
        assert_eq!(&body[..json.len()], json.as_bytes());
        assert_eq!(&body[json.len()..json.len() + 4], &6u32.to_be_bytes());
        assert_eq!(&body[json.len() + 4..], b"BUNDLE");
        assert_eq!(frame(&head, &vec![0; MAX_BUNDLE + 1]).err().unwrap().code, "too_large");
    }
}
