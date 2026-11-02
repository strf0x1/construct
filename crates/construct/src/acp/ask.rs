//! `construct acp` asking its client: a permission question goes out as
//! `session/request_permission`, and the option the client picks comes back
//! as the [`Decision`]. The server actor holds requests in flight. Each
//! session's `Perms` asks through a `ClientAsk` naming it.
//!
//! The design choice: every option id is a verdict (`once`, `session`,
//! `always`, `deny`, and `folder-session`/`folder-always` when a folder is on
//! offer), so an answer needs no table, and anything else (an unknown id, an
//! error, a client gone) denies. The request's `toolCallId` names the
//! question, not the tool call: `Perms` is not told which call is asking.

use super::live::Command;
use super::rpc::RpcError;
use crate::permission::{Access, Ask, Decision, Request, Verdict};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

pub(super) type Answer = Result<Value, RpcError>;
static NEXT_QUESTION: AtomicU64 = AtomicU64::new(1);

/// A stable internal question key survives replacement of its wire id.
struct Forget(mpsc::UnboundedSender<Command>, u64);

impl Drop for Forget {
    fn drop(&mut self) {
        let _ = self.0.send(Command::Forget(self.1));
    }
}

/// One session's way of asking the server actor to question its client.
pub(super) struct ClientAsk {
    session: String,
    generation: u64,
    commands: mpsc::UnboundedSender<Command>,
}

impl ClientAsk {
    pub fn new(session: String, generation: u64, commands: mpsc::UnboundedSender<Command>) -> Self {
        Self { session, generation, commands }
    }
}

#[async_trait::async_trait]
impl Ask for ClientAsk {
    async fn ask(&self, request: Request) -> Decision {
        let key = NEXT_QUESTION.fetch_add(1, Ordering::Relaxed);
        let (answer, rx) = oneshot::channel();
        let _forget = Forget(self.commands.clone(), key);
        let command = Command::Ask { key, session: self.session.clone(), generation: self.generation, request, answer };
        if self.commands.send(command).is_err() {
            return Decision::DENY;
        }
        match rx.await {
            Ok(Ok(result)) => decision(&result),
            _ => Decision::DENY,
        }
    }
}

/// `session/request_permission`'s params for question `id`.
pub fn params(session: &str, id: u64, req: &Request) -> Value {
    json!({
        "sessionId": session,
        "toolCall": {
            "toolCallId": format!("ask-{id}"),
            "title": title(req),
            "kind": kind(req),
            "status": "pending",
            "rawInput": serde_json::to_value(req).unwrap_or(Value::Null),
        },
        "options": options(req),
    })
}

fn title(req: &Request) -> String {
    let who = |agent: &str| if agent.is_empty() { String::new() } else { format!("{agent}: ") };
    match req {
        Request::Path(p) => {
            let rerun = if p.rerun { " (allowing runs the command again)" } else { "" };
            format!("{}{} access to {} for {}{rerun}", who(&p.agent), p.access.word(), p.paths.join(", "), p.tool)
        }
        Request::Tool(t) => format!("{}{}", who(&t.agent), t.title),
        Request::Operation(o) => format!("{}{} ({})", who(&o.agent), o.operation, o.scope),
    }
}

/// ACP's tool kind, so a client picks the right icon.
fn kind(req: &Request) -> &str {
    match req {
        Request::Path(p) if p.rerun => "execute",
        Request::Path(p) if p.access == Access::Write => "edit",
        Request::Path(_) => "read",
        Request::Tool(t) => t.kind.as_deref().unwrap_or("other"),
        Request::Operation(_) => "execute",
    }
}

/// The options offered, in `Request::offers`' order; a folder on offer adds
/// a folder-wide variant after each remembered answer.
pub fn options(req: &Request) -> Vec<Value> {
    let option = |id: &str, name: String, kind: &str| json!({ "optionId": id, "name": name, "kind": kind });
    let mut out = vec![];
    for v in req.offers() {
        match v {
            Verdict::Once => out.push(option("once", "Allow once".into(), "allow_once")),
            Verdict::Session => {
                out.push(option("session", "Allow for this session".into(), "allow_always"));
                if let Some(f) = req.folder() {
                    out.push(option("folder-session", format!("Allow {f} for this session"), "allow_always"));
                }
            }
            Verdict::Always => {
                out.push(option("always", "Always allow (saved to config.toml)".into(), "allow_always"));
                if let Some(f) = req.folder() {
                    out.push(option(
                        "folder-always",
                        format!("Always allow {f} (saved to config.toml)"),
                        "allow_always",
                    ));
                }
            }
            Verdict::Deny => out.push(option("deny", "Deny".into(), "reject_once")),
            Verdict::Cancel => {}
        }
    }
    out
}

/// The decision a `session/request_permission` result carries.
pub fn decision(result: &Value) -> Decision {
    let outcome = &result["outcome"];
    if outcome["outcome"] == "cancelled" {
        return Decision::CANCEL;
    }
    let (verdict, folder) = match outcome["optionId"].as_str() {
        Some("once") => (Verdict::Once, false),
        Some("session") => (Verdict::Session, false),
        Some("always") => (Verdict::Always, false),
        Some("folder-session") => (Verdict::Session, true),
        Some("folder-always") => (Verdict::Always, true),
        _ => return Decision::DENY,
    };
    Decision { verdict, folder }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{PathAsk, ToolAsk};

    fn path(folder: Option<&str>, no_always: bool) -> Request {
        Request::Path(PathAsk {
            agent: String::new(),
            tool: "read".into(),
            access: Access::Read,
            paths: vec!["/elsewhere/a.txt".into()],
            folder: folder.map(str::to_string),
            detail: String::new(),
            rerun: false,
            ahead: false,
            no_always,
        })
    }

    fn ids(req: &Request) -> Vec<String> {
        options(req).iter().map(|o| o["optionId"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn the_options_are_what_the_tui_would_offer() {
        assert_eq!(ids(&path(None, false)), ["once", "session", "always", "deny"]);
        assert_eq!(
            ids(&path(Some("/elsewhere"), false)),
            ["once", "session", "folder-session", "always", "folder-always", "deny"]
        );
        assert_eq!(ids(&path(Some("/elsewhere"), true)), ["once", "session", "folder-session", "deny"]);
        let tool = Request::Tool(ToolAsk {
            agent: "scout".into(),
            call_id: "c1".into(),
            title: "rm -rf build".into(),
            kind: Some("execute".into()),
            can_allow: false,
            can_reject: true,
        });
        assert_eq!(ids(&tool), ["deny"]);
        let p = params("s1", 4, &tool);
        assert_eq!(p["toolCall"]["toolCallId"], "ask-4");
        assert_eq!(p["toolCall"]["title"], "scout: rm -rf build");
        assert_eq!(p["toolCall"]["kind"], "execute");
        assert_eq!(p["toolCall"]["rawInput"]["type"], "tool");
    }

    #[test]
    fn an_answer_is_read_from_its_option_id_and_anything_else_denies() {
        let picked = |id: &str| decision(&json!({ "outcome": { "outcome": "selected", "optionId": id } }));
        assert_eq!(picked("once"), Decision { verdict: Verdict::Once, folder: false });
        assert_eq!(picked("folder-always"), Decision { verdict: Verdict::Always, folder: true });
        assert_eq!(picked("y"), Decision::DENY);
        assert_eq!(decision(&json!({ "outcome": { "outcome": "cancelled" } })), Decision::CANCEL);
        assert_eq!(decision(&json!({})), Decision::DENY);
    }
}
