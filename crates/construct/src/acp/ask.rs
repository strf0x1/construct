//! `construct acp` asking its client: a permission question goes out as
//! `session/request_permission`, and the option the client picks comes back
//! as the [`Decision`]. One [`Questions`] per server holds the requests in
//! flight; each session's `Perms` asks through a [`ClientAsk`] naming it.
//!
//! The design choice: every option id is a verdict (`once`, `session`,
//! `always`, `deny`, and `folder-session`/`folder-always` when a folder is on
//! offer), so an answer needs no table, and anything else (an unknown id, an
//! error, a client gone) denies. The request's `toolCallId` names the
//! question, not the tool call: `Perms` is not told which call is asking.

use super::rpc::{self, RpcError};
use crate::permission::{Access, Ask, Decision, Request, Verdict};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender};
use tokio::sync::oneshot;

type Answer = Result<Value, RpcError>;

/// The server's requests to its client that wait for a response.
pub struct Questions {
    /// Weak, as `Upstream`'s is: the writer ends when the server's sender goes.
    out: WeakUnboundedSender<String>,
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
}

impl Questions {
    pub fn new(out: &UnboundedSender<String>) -> Arc<Self> {
        Arc::new(Self { out: out.downgrade(), next: AtomicU64::new(1), waiting: Mutex::new(HashMap::new()) })
    }

    fn waiting(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Answer>>> {
        // A poisoned lock means a panic mid-insert; the map is still usable.
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The client's response to request `id`. One nobody waits for (the
    /// question was cancelled, or the id was never ours) is dropped.
    pub fn answer(&self, id: &Value, result: Answer) {
        let tx = id.as_u64().and_then(|n| self.waiting().remove(&n));
        if let Some(tx) = tx {
            let _ = tx.send(result);
        }
    }

    /// The client is gone: everything still waiting is denied.
    pub fn close(&self) {
        self.waiting().clear();
    }
}

/// Takes a question off the waiting list however its asker ends, including
/// being dropped when the turn is cancelled.
struct Forget<'a>(&'a Questions, u64);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.0.waiting().remove(&self.1);
    }
}

/// One session's way of asking the client.
pub struct ClientAsk {
    session: String,
    questions: Arc<Questions>,
}

impl ClientAsk {
    pub fn new(session: String, questions: Arc<Questions>) -> Self {
        Self { session, questions }
    }
}

#[async_trait::async_trait]
impl Ask for ClientAsk {
    async fn ask(&self, req: Request) -> Decision {
        let q = &*self.questions;
        let id = q.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        q.waiting().insert(id, tx);
        let _forget = Forget(q, id);
        let line = rpc::request(id, "session/request_permission", params(&self.session, id, &req));
        if q.out.upgrade().is_none_or(|out| out.send(line).is_err()) {
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
    use tokio::sync::mpsc;

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

    #[tokio::test]
    async fn a_question_waits_for_its_response_and_is_forgotten_when_dropped() {
        let (out, mut lines) = mpsc::unbounded_channel();
        let q = Questions::new(&out);
        let ask = ClientAsk::new("s1".into(), q.clone());
        let asking = tokio::spawn(async move { ask.ask(path(None, false)).await });
        let sent: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
        assert_eq!(
            (sent["method"].as_str(), sent["params"]["sessionId"].as_str()),
            (Some("session/request_permission"), Some("s1"))
        );
        q.answer(&sent["id"], Ok(json!({ "outcome": { "outcome": "selected", "optionId": "session" } })));
        assert_eq!(asking.await.unwrap().verdict, Verdict::Session);

        // Cancelled mid-question: nothing is left waiting, and a late answer is dropped.
        let ask = ClientAsk::new("s1".into(), q.clone());
        let asking = tokio::spawn(async move { ask.ask(path(None, false)).await });
        let sent: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
        asking.abort();
        let _ = asking.await;
        assert!(q.waiting().is_empty());
        q.answer(&sent["id"], Ok(json!({})));

        // The client going away denies what was still waiting.
        let ask = ClientAsk::new("s1".into(), q.clone());
        let asking = tokio::spawn(async move { ask.ask(path(None, false)).await });
        lines.recv().await.unwrap();
        q.close();
        assert_eq!(asking.await.unwrap(), Decision::DENY);
    }
}
