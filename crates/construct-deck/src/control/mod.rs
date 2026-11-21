//! The `deck` tool: wire shaping, the HTTP client, the listing summaries and
//! the `ToolSource` the composition root registers. See the crate docs for the
//! design choice.

mod client;
mod integration;
mod summary;

pub use client::{MAX_BODY, MAX_REPLY, PATH, REFUSALS, refusal};
pub use integration::{MAX_CONTRIBUTORS, MAX_INTEGRATION_REASON};

use construct_core::config::ToolsConfig;
use construct_core::tools::{Ctx, Deck, Display, Group, Output, Tool, ToolError, ToolKind, ToolSource, def};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The tool's wire name.
pub const NAME: &str = "deck";
const INTEGRATE: &str = "integrate";

/// The operations, in the contract's order (§13.3).
pub const OPERATIONS: &[&str] = &["tasks", "task", "create", INTEGRATE, "prompt", "wait", "stop"];

/// The longest `wait_ms` Deck accepts; a larger one is refused here, never
/// shortened (§13.3).
pub const MAX_WAIT_MS: u64 = 300_000;
/// `wait_ms` when the model gives none.
const DEFAULT_WAIT_MS: u64 = 120_000;
/// How long past the server's own wait the HTTP exchange may take.
const GRACE: Duration = Duration::from_secs(15);
/// `prompt` may wait up to 60 s for a new task's session (§13.3).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(75);
/// `stop` returns once the stop is confirmed or has failed.
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(30);

const DESCRIPTION: &str = "Direct this coordinator's worker tasks through Deck. Use tasks to list your projects and scoped tasks, or task for one summary and its last message. Use create to start a worker with a brief. Use integrate to combine contributor tasks in order on a separate review branch. Deck pins the commits and reports missing inputs through task or wait. For create and integrate, omit project if you have one project. Use prompt to send text to an idle task, wait for a turn to end or a task to need attention, and stop to stop the agent and keep its record. After prompt returns turn n, use after_turn n-1 to wait for it. Deck decides what you may do. Report refusals and requests for a human decision.";

/// The fields each operation takes, required first (§13.3).
fn fields(operation: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match operation {
        "tasks" => (&[], &["project"]),
        "task" | "stop" => (&["id"], &[]),
        "create" => (&["brief"], &["project", "title", "budget"]),
        INTEGRATE => (&["destination", "contributors", "brief"], &["project", "title", "budget"]),
        "prompt" => (&["id", "text"], &[]),
        "wait" => (&["id"], &["after_turn", "wait_ms"]),
        _ => return None,
    })
}

/// The wire body for a call, or why it cannot be sent.
pub fn request(args: &Value) -> Result<Value, ToolError> {
    let bad = |m: String| ToolError::new("invalid_argument", m);
    let Some(map) = args.as_object() else { return Err(bad("arguments must be an object".into())) };
    let operation = map.get("operation").and_then(Value::as_str).unwrap_or_default();
    let Some((required, optional)) = fields(operation) else {
        return Err(bad(format!("operation must be one of {}", OPERATIONS.join(", "))));
    };
    let mut body = Map::new();
    body.insert("operation".into(), json!(operation));
    for (key, value) in map {
        if key == "operation" || value.is_null() {
            continue;
        }
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            let takes = [required, optional].concat().join(", ");
            return Err(bad(format!("{operation} does not take {key}; it takes {takes}")));
        }
        body.insert(key.clone(), check(key, value).map_err(|m| bad(format!("{key}: {m}")))?);
    }
    if let Some(missing) = required.iter().find(|k| !body.contains_key(**k)) {
        return Err(bad(format!("{operation} needs {missing}")));
    }
    if operation == "wait" && !body.contains_key("wait_ms") {
        body.insert("wait_ms".into(), json!(DEFAULT_WAIT_MS));
    }
    Ok(Value::Object(body))
}

/// One field's value as the wire takes it.
fn check(key: &str, value: &Value) -> Result<Value, String> {
    let text = || value.as_str().filter(|s| !s.is_empty()).map(|s| json!(s)).ok_or("must be a non-empty string");
    Ok(match key {
        "after_turn" => json!(value.as_u64().ok_or("must be a whole number")?),
        "wait_ms" => {
            let ms = value.as_u64().ok_or("must be a whole number of milliseconds")?;
            if ms > MAX_WAIT_MS {
                return Err(format!("is at most {MAX_WAIT_MS}; wait again for longer"));
            }
            json!(ms)
        }
        "budget" => {
            let map = value.as_object().filter(|m| !m.is_empty()).ok_or("must name requests and/or tokens")?;
            let mut out = Map::new();
            for (k, v) in map {
                if k != "requests" && k != "tokens" {
                    return Err(format!("takes only requests and tokens, not {k}"));
                }
                out.insert(k.clone(), json!(v.as_u64().filter(|n| *n > 0).ok_or("is a positive whole number")?));
            }
            Value::Object(out)
        }
        "destination" => integration::destination(value)?,
        "contributors" => integration::contributors(value)?,
        _ => text()?,
    })
}

/// How long the exchange for `body` may take: always longer than a wait.
fn timeout(body: &Value) -> Duration {
    match body["operation"].as_str() {
        Some("wait") => Duration::from_millis(body["wait_ms"].as_u64().unwrap_or(0)) + GRACE,
        Some("prompt") => PROMPT_TIMEOUT,
        Some("stop") => STOP_TIMEOUT,
        _ => TIMEOUT,
    }
}

/// The `deck` tool, bound to one gateway.
pub struct Control {
    url: String,
    /// A fixed token, or `None` to read `DECK_CONTROL_TOKEN` per request.
    token: Option<Option<String>>,
    /// Set once Deck refuses the token: it comes from this process's
    /// environment, so every later request would be refused the same way.
    refused_token: AtomicBool,
    /// The last body refused as malformed: the same body is refused again.
    bad_body: Mutex<Option<String>>,
}

impl Control {
    /// The tool for the gateway at `url`, its token read from the environment.
    pub fn new(url: &str) -> Self {
        Self { url: url.to_string(), token: None, refused_token: AtomicBool::new(false), bad_body: Mutex::new(None) }
    }

    /// The tool with a fixed token (or none) instead of the environment's,
    /// for a harness that cannot change its own environment.
    pub fn with_token(url: &str, token: Option<String>) -> Self {
        Self { token: Some(token), ..Self::new(url) }
    }

    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    fn bad_body(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.bad_body.lock().unwrap()
    }

    /// Send `body`, refusing locally what Deck would refuse unchanged.
    async fn send(&self, ctx: &Ctx, body: &Value) -> Result<Value, ToolError> {
        if self.refused_token.load(Ordering::Relaxed) {
            return Err(ToolError::new(
                "deck_control_unauthorized",
                "Deck already refused this coordinator's control token; nothing was sent. Stop using the deck tool and tell the user.",
            ));
        }
        let text = body.to_string();
        if self.bad_body().as_deref() == Some(&text) {
            return Err(ToolError::new(
                "deck_control_bad_request",
                "Deck refused exactly this request as malformed a moment ago; nothing was sent. Change it.",
            ));
        }
        let operation = body["operation"].as_str().unwrap_or_default();
        let writes = matches!(operation, "create" | INTEGRATE | "prompt" | "stop");
        let mut cancel = ctx.cancel.clone();
        if *cancel.borrow() {
            return Err(ToolError::new("interrupted", "interrupted by the user before it was sent"));
        }
        let token = self.token.clone().unwrap_or_else(client::env_token);
        let fut = client::send(&self.url, body, timeout(body), writes, token.as_deref());
        tokio::pin!(fut);
        let result = loop {
            tokio::select! {
                r = &mut fut => break r,
                changed = cancel.changed() => {
                    // No sender left: nothing can cancel it now.
                    if changed.is_err() {
                        break (&mut fut).await;
                    }
                    if *cancel.borrow() {
                        let next = if writes { " Its outcome is unknown: read the task back before repeating it." } else { "" };
                        return Err(ToolError::new("interrupted", format!("deck {operation} was interrupted by the user.{next}")));
                    }
                }
            }
        };
        match &result {
            Err(e) if e.code == "deck_control_unauthorized" => self.refused_token.store(true, Ordering::Relaxed),
            Err(e) if e.code == "deck_control_bad_request" => *self.bad_body() = Some(text),
            _ => {}
        }
        result
    }
}

#[async_trait::async_trait]
impl Tool for Control {
    fn name(&self) -> &str {
        NAME
    }
    fn group(&self) -> Group {
        Group::Coordinator
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }
    fn display(&self) -> Display {
        Display { primary_arg: "operation", preview_lines: 6 }
    }
    fn definition(&self) -> Value {
        let text = |d: &str| json!({ "type": "string", "description": d });
        let count = |d: &str, min: u64| json!({ "type": "integer", "minimum": min, "description": d });
        def(
            NAME,
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": OPERATIONS },
                    "project": text("tasks filters to this project. For create and integrate, leave it out unless you have several projects. deck tasks lists your project IDs."),
                    "id": text("task, prompt, wait, stop: the task id."),
                    "brief": text("create or integrate: the worker's first prompt. Include the checks the worker should run."),
                    "title": { "type": "string", "maxLength": 80, "description": "create or integrate: the task's title." },
                    "budget": {
                        "type": "object",
                        "description": "create or integrate: a lower budget than your policy's per-worker budget.",
                        "properties": { "requests": { "type": "integer", "minimum": 1 }, "tokens": { "type": "integer", "minimum": 1 } },
                        "additionalProperties": false
                    },
                    "destination": integration::destination_schema(),
                    "contributors": {
                        "type": "array", "minItems": 1, "maxItems": MAX_CONTRIBUTORS, "uniqueItems": true,
                        "items": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "description": "integrate: contributor task IDs in the order to combine their commits."
                    },
                    "text": text("prompt: what to tell the idle task."),
                    "after_turn": count("wait: return once a turn numbered above this ends (default 0).", 0),
                    "wait_ms": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "description": "wait: the longest to block, in ms (default 120000)." }
                },
                "required": ["operation"],
                "additionalProperties": false
            }),
        )
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        let body = request(&args)?;
        let reply = self.send(ctx, &body).await?;
        let (text, data) = summary::reply(&body, &reply);
        Ok(Output::plain(construct_core::tools::cap_output(ctx, NAME, &text)).with_data(data))
    }
}

/// What the composition root registers: the `deck` tool, for a task with a
/// control gateway.
pub struct DeckTools;

impl ToolSource for DeckTools {
    fn tools(&self, _cfg: &ToolsConfig, deck: &Deck) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
        Ok(deck.control.iter().map(|url| Arc::new(Control::new(url)) as Arc<dyn Tool>).collect())
    }
}

#[cfg(test)]
mod tests;
