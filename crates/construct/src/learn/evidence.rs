//! The two kinds of evidence a session holds that a digest buries: what the
//! user said after the first request (a follow-up is almost always a
//! correction or a hint), and each failed tool call paired with the next call
//! of the same tool that worked. The pair is the lesson: the failure alone
//! says what broke, the contrast says what fixed it (ReasoningBank, ExpeL).
use crate::llm::{self, Message};
use std::fmt::Write as _;

/// A user turn after the first, with what the assistant had said just before.
pub struct FollowUp {
    pub after: String,
    pub user: String,
}

/// A failed call and the next successful call of the same tool, if any.
pub struct Contrast {
    pub failed: String,
    pub error: String,
    pub fixed: Option<String>,
}

pub fn followups(messages: &[Message]) -> Vec<FollowUp> {
    let mut out = Vec::new();
    let mut seen_user = false;
    let mut last_said = String::new();
    for m in messages {
        match m.role.as_str() {
            "assistant" => {
                if let Some(c) = m.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    last_said = c.to_string();
                }
            }
            "user" => {
                let text = m.content.as_deref().unwrap_or("").split("<system-reminder>").next().unwrap_or("").trim();
                if text.is_empty() {
                    continue;
                }
                if seen_user {
                    out.push(FollowUp { after: llm::truncate(&last_said, 300), user: llm::truncate(text, 600) });
                }
                seen_user = true;
            }
            _ => {}
        }
    }
    out
}

pub fn contrasts(messages: &[Message]) -> Vec<Contrast> {
    // (tool name, call text) by call id, in call order.
    let mut calls: Vec<(String, String, String)> = Vec::new();
    for m in messages {
        for c in &m.tool_calls {
            calls.push((c.id.clone(), c.function.name.clone(), llm::truncate(&c.function.arguments, 240)));
        }
    }
    let result = |id: &str| {
        messages
            .iter()
            .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(id))
            .and_then(|m| m.content.as_deref())
    };
    let mut out = Vec::new();
    for (i, (id, name, args)) in calls.iter().enumerate() {
        let Some(err) = result(id).filter(|t| failed(t)) else { continue };
        let fixed = calls[i + 1..]
            .iter()
            .find(|(id2, name2, _)| name2 == name && result(id2).is_some_and(|t| !failed(t)))
            .map(|(_, n, a)| format!("{n} {a}"));
        out.push(Contrast { failed: format!("{name} {args}"), error: llm::truncate(tail(err, 240), 240), fixed });
    }
    out
}

/// What says the environment, not the work, went wrong. A test that fails
/// is the task in progress; a missing command or file is a lesson.
const ENV_FAILURES: &[&str] =
    &["No such file or directory", "command not found", "No module named", "Permission denied", "cannot access"];

/// A failed call: an error result, or output that shows an environment
/// failure although the command exited 0 (`cmd 2>&1 | tail` hides the exit
/// status, and that is how a stale command goes unnoticed).
pub fn failed(result: &str) -> bool {
    result.starts_with("Error:") || ENV_FAILURES.iter().any(|m| result.contains(m))
}

/// The end of an error: a traceback's last lines say what went wrong.
fn tail(s: &str, n: usize) -> &str {
    crate::text::tail(s, n)
}

/// Both, as a prompt section; empty when the session has neither.
pub fn section(messages: &[Message]) -> String {
    let mut s = String::new();
    let f = followups(messages);
    if !f.is_empty() {
        let _ = writeln!(
            s,
            "USER FOLLOW-UPS (what the user had to add after the first request; usually a correction or a hint):"
        );
        for x in &f {
            let _ = writeln!(s, "- after the assistant said {:?}\n  the user said {:?}", x.after, x.user);
        }
    }
    let c = contrasts(messages);
    if !c.is_empty() {
        let _ = writeln!(s, "FAILED CALLS, each with the next call of the same tool that worked:");
        for x in c.iter().take(8) {
            match &x.fixed {
                Some(fix) => {
                    let _ = writeln!(s, "- failed: {}\n  error: {}\n  then worked: {fix}", x.failed, x.error);
                }
                None => {
                    let _ = writeln!(
                        s,
                        "- failed: {}\n  error: {}\n  (no later call of that tool worked)",
                        x.failed, x.error
                    );
                }
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn call(id: &str, args: &str) -> Message {
        let mut m = Message::text("assistant", "");
        m.content = None;
        m.tool_calls = vec![ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall { name: "shell".into(), arguments: args.into() },
        }];
        m
    }

    #[test]
    fn pairs_a_failure_with_the_next_success_of_the_same_tool() {
        let msgs = vec![
            Message::user("add median"),
            call("a", "python -m unittest"),
            Message::tool("a", "Error: ModuleNotFoundError: No module named 'tally'\n[exit 1]"),
            call("b", "./tools/test"),
            Message::tool("b", "OK"),
            Message::text("assistant", "done"),
            Message::user("you forgot the changelog"),
        ];
        let c = contrasts(&msgs);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].fixed.as_deref(), Some("shell ./tools/test"));
        assert!(c[0].error.contains("No module named"));
        let f = followups(&msgs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].after, "done");
        assert!(section(&msgs).contains("then worked: shell ./tools/test"));
    }

    #[test]
    fn a_failure_hidden_by_a_pipe_still_counts_but_a_red_test_does_not() {
        let msgs = vec![
            Message::user("add median"),
            call("a", "./tools/test 2>&1 | tail -3"),
            Message::tool("a", "bash: ./tools/test: No such file or directory"),
            call("b", "make test 2>&1 | tail -3"),
            Message::tool("b", "FAILED (failures=1)"),
            call("c", "make test"),
            Message::tool("c", "OK"),
        ];
        let c = contrasts(&msgs);
        assert_eq!(c.len(), 1, "only the missing command is a failure");
        assert_eq!(c[0].fixed.as_deref(), Some("shell make test 2>&1 | tail -3"));
    }

    #[test]
    fn a_plain_session_has_no_evidence() {
        let msgs = vec![Message::user("hi"), Message::text("assistant", "hello")];
        assert!(section(&msgs).is_empty());
    }
}
