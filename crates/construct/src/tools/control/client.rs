//! The HTTP side of Deck's coordinator task control (contract 1.5.0 §13):
//! one bounded JSON operation per `POST <DECK_CONTROL_URL>/control`, and
//! every refusal turned into prose that says what the model should change.
//!
//! The design choice: the token is read from `DECK_CONTROL_TOKEN` as each
//! request is sent (`env_token`) and goes into a header marked sensitive,
//! nowhere else; it
//! is never stored, logged, returned or put in an error. A refusal's `code`
//! and `retryable` decide the wording, and nothing here retries: a refusal is
//! the host's answer, never a network outage.

use crate::config::deck::CONTROL_TOKEN;
use crate::tools::ToolError;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::{Value, json};
use std::time::Duration;

/// The one path the gateway serves (§13.3).
pub const PATH: &str = "/control";
/// The largest request body (§13.3); a larger one is refused here.
pub const MAX_BODY: usize = 32 * 1024;
/// The largest reply body (§13.4); more is not read.
pub const MAX_REPLY: usize = 1024 * 1024;

/// Each refusal code (§13.5), whether it may succeed later, and what the
/// model should do about it.
pub const REFUSALS: &[(&str, bool, &str)] = &[
    (
        "deck_control_bad_request",
        false,
        "The request is malformed. Fix it; sending it again unchanged is refused the same way.",
    ),
    (
        "deck_control_unauthorized",
        false,
        "Deck refused this coordinator's control token (DECK_CONTROL_TOKEN), or none was sent. Retrying cannot fix it: stop using the deck tool and tell the user.",
    ),
    (
        "deck_control_scope",
        false,
        "The project is outside this coordinator's project set; Deck's message names the projects you may use. Choose one of those (or leave project out if you have only one), or report it: only the user can widen the set, in Deck.",
    ),
    (
        "deck_control_cap",
        true,
        "This coordinator is at its worker cap or hourly creation rate. Wait for a worker to finish (deck wait) or for the hour to roll, then try again; only the user can raise the cap.",
    ),
    (
        "deck_control_refused",
        false,
        "Deck will not do this as asked (an operation it does not serve, a budget above the ceiling, a project with no agent command, a stopped task, or a coordinator). Do something else; do not resend it unchanged.",
    ),
    (
        "deck_control_unknown",
        false,
        "No such task in this coordinator's scope. List tasks (deck tasks) to find the right id.",
    ),
    (
        "deck_control_busy",
        true,
        "The task's turn has not ended. Call deck wait with its id until the turn is idle, then prompt again.",
    ),
    (
        "deck_control_unavailable",
        true,
        "Deck's gateway could not reach deckd. Retry later; do not look for another route to Deck.",
    ),
];

/// One request to the gateway at `base`. `timeout` bounds the whole exchange
/// and must exceed any server-side wait the request asks for.
/// `token` is the bearer token, if this coordinator has one.
pub async fn send(
    base: &str,
    body: &Value,
    timeout: Duration,
    writes: bool,
    token: Option<&str>,
) -> Result<Value, ToolError> {
    let bytes = body.to_string().into_bytes();
    if bytes.len() > MAX_BODY {
        return Err(ToolError::new(
            "too_large",
            format!(
                "this request is {} bytes; Deck's control gateway takes at most {MAX_BODY}. Shorten the brief or text.",
                bytes.len()
            ),
        ));
    }
    // Loopback: a proxy variable must not send it elsewhere.
    let http = reqwest::Client::builder()
        .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
        .map_err(|_| ToolError::new("deck_control", "could not start the client for Deck's control gateway"))?;
    let mut request =
        http.post(format!("{base}{PATH}")).header("content-type", "application/json").body(bytes).timeout(timeout);
    if let Some(header) = bearer(token)? {
        request = request.header(AUTHORIZATION, header);
    }
    let mut response = request.send().await.map_err(|error| lost(error.is_timeout(), writes))?;
    let status = response.status().as_u16();
    let mut text = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| lost(error.is_timeout(), writes))? {
        if text.len() + chunk.len() > MAX_REPLY {
            return Err(ToolError::new(
                "deck_control",
                format!("Deck's control gateway sent more than {MAX_REPLY} bytes; the reply was dropped."),
            ));
        }
        text.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) && value.is_object() {
        return Ok(value);
    }
    Err(refusal(status, &value, writes))
}

/// `DECK_CONTROL_TOKEN`, read as a request is sent (§13.2).
pub fn env_token() -> Option<String> {
    std::env::var(CONTROL_TOKEN).ok().filter(|t| !t.is_empty())
}

/// `Authorization: Bearer <token>` when there is a token, marked sensitive so
/// no debug output prints it.
fn bearer(token: Option<&str>) -> Result<Option<HeaderValue>, ToolError> {
    let Some(token) = token else { return Ok(None) };
    let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        ToolError::new(
            "deck_control_unauthorized",
            "DECK_CONTROL_TOKEN is not a usable bearer token, so nothing was sent. Stop using the deck tool and tell the user.",
        )
    })?;
    value.set_sensitive(true);
    Ok(Some(value))
}

/// No reply at all: for a write the outcome is unknown, never "refused".
fn lost(timed_out: bool, writes: bool) -> ToolError {
    let what = if timed_out { "timed out" } else { "could not be reached" };
    let next = if writes {
        "The outcome is unknown: read the task back (deck tasks or deck task) before repeating it."
    } else {
        "Nothing changed; try again shortly."
    };
    ToolError::new("deck_control_network", format!("Deck's control gateway {what}. {next}"))
        .with_data(json!({ "code": "network", "retryable": true }))
}

/// A refusal as the model reads it (§13.5): Deck's message, the code, and
/// what to change.
pub fn refusal(status: u16, body: &Value, writes: bool) -> ToolError {
    let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or_default();
    let message = body.pointer("/error/message").and_then(Value::as_str).unwrap_or("no reason given");
    let known = REFUSALS.iter().find(|(c, _, _)| *c == code);
    let retryable = body.get("retryable").and_then(Value::as_bool).or(known.map(|k| k.1)).unwrap_or(false);
    let data = json!({ "status": status, "code": code, "retryable": retryable });
    let (tool_code, next) = match known {
        Some((c, _, next)) => (*c, (*next).to_string()),
        None if code.is_empty() => {
            let next = if writes {
                "The reply was not Deck's refusal shape; the outcome is unknown, so read the task back before repeating it."
            } else {
                "The reply was not Deck's refusal shape."
            };
            ("deck_control", next.to_string())
        }
        None if retryable => ("deck_control", "It may succeed later; retry once the cause has passed.".into()),
        None => ("deck_control", "Do not resend it unchanged.".into()),
    };
    let label = if code.is_empty() { format!("HTTP {status}") } else { format!("{code} ({status})") };
    ToolError::new(tool_code, format!("Deck refused: {label}: {message}. {next}")).with_data(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_code_keeps_deck_s_retryable_and_a_bare_status_is_unknown() {
        let body = json!({ "error": { "message": "new", "type": "deck_control_new", "code": "deck_control_new" }, "retryable": true });
        let e = refusal(429, &body, true);
        assert_eq!(e.code, "deck_control");
        assert!(e.message.contains("deck_control_new (429): new") && e.message.contains("retry once"), "{}", e.message);
        let e = refusal(502, &Value::Null, true);
        assert!(e.message.contains("HTTP 502") && e.message.contains("outcome is unknown"), "{}", e.message);
        assert!(!refusal(502, &Value::Null, false).message.contains("outcome is unknown"));
    }

    #[test]
    fn a_lost_write_is_unknown_and_a_lost_read_is_not() {
        assert!(lost(true, true).message.contains("timed out") && lost(true, true).message.contains("unknown"));
        assert!(lost(false, false).message.contains("Nothing changed"));
    }
}
