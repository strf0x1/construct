//! Fitting history into the context window. The model's context is a view of
//! the journal, and a cut only changes the view: old tool results become
//! stubs (`elide`), and if that is not enough, older rounds become one summary
//! message (`summarize`). A `Cut` is journaled with the `compacted` event and
//! replayed by the projector, so resume rebuilds the same view.
//!
//! The one choice a reader must know: the unit is the round, an assistant
//! message and its tool results. Cuts land on round boundaries, so a call is
//! never separated from its result, and a single long user turn can be cut
//! as well as a long conversation.

mod summary;

pub use summary::{SYSTEM as SUMMARY_SYSTEM, request as summary_request};

use crate::config::CompactionConfig;
use crate::llm::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Results shorter than this are kept: their stub would save almost nothing.
const MIN_ELIDE: usize = 600;
const ELIDED: &str = "[elided to save context:";
const SUMMARY_HEAD: &str = "<system-reminder>\nThe conversation so far was compacted to fit the context window. Summary of the earlier work:\n\n";
const SUMMARY_TAIL: &str = "\n</system-reminder>";
const REQUEST_HEAD: &str = "\n\nThe request being worked on (verbatim):\n\n";
const FILLER: &str = "Understood; continuing from that summary.";

/// What a cut did to the view, enough to replay it on the same messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Cut {
    /// Call ids whose results became stubs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elide: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    /// Messages `1..upto` (after eliding) were replaced by the summary.
    pub upto: usize,
    pub text: String,
    /// The user's latest request, when it was among the replaced messages:
    /// pinned verbatim under the summary rather than paraphrased by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Elide,
    Summarize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The estimate passed the threshold.
    Threshold,
    /// The provider rejected the prompt as too long.
    Overflow,
    /// Results passed `age_rounds`; nothing size-driven.
    Age,
}

impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Reason::Threshold => "threshold",
            Reason::Overflow => "overflow",
            Reason::Age => "age",
        }
    }
}

/// What the planner chose. `upto` is set for a summary; the text comes from
/// the summarizer, after which `Cut` is complete.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub mode: Mode,
    pub reason: Reason,
    pub elide: Vec<String>,
    pub upto: Option<usize>,
    pub request: Option<String>,
    pub tokens_before: u64,
    /// The estimate after the cut (for a summary, with `summary_allowance`).
    pub tokens_after: u64,
    /// Whether the cut reaches the target.
    pub reached: bool,
}

impl Plan {
    pub fn cut(&self, summary: Option<String>) -> Cut {
        Cut {
            elide: self.elide.clone(),
            summary: match (self.upto, summary) {
                (Some(upto), Some(text)) => Some(Summary { upto, text, request: self.request.clone() }),
                _ => None,
            },
        }
    }
}

/// The token budget for the prompt: the window less what the answer may use.
pub fn budget(window: u64, max_tokens: Option<u32>) -> u64 {
    let reserve = max_tokens.map(u64::from).unwrap_or((window / 8).min(8192));
    window.saturating_sub(reserve).max(1024)
}

/// Output tokens the summarizer may spend beyond the summary itself:
/// reasoning models think before they write.
pub const REASONING_HEADROOM: u32 = 8192;

/// Tokens a summary is assumed to take while planning, before it exists.
pub fn summary_allowance(budget: u64) -> u64 {
    (budget / 10).min(4000)
}

/// Decide whether and how to cut. `size` estimates the request's prompt
/// tokens for a candidate message list (tools and system included).
/// `force` plans a cut even under the threshold (overflow recovery).
pub fn plan(
    msgs: &[Message],
    size: &dyn Fn(&[Message]) -> u64,
    budget: u64,
    policy: &CompactionConfig,
    reason: Reason,
) -> Option<Plan> {
    let before = size(msgs);
    let hard = (policy.threshold as f64 * budget as f64) as u64;
    let target = (policy.target as f64 * budget as f64) as u64;
    let rounds = rounds(msgs);
    if reason != Reason::Overflow && before < hard {
        if policy.age_rounds == 0 {
            return None;
        }
        let ids = elidable(msgs, &rounds, policy.age_rounds);
        if ids.is_empty() {
            return None;
        }
        let after = size(&apply_elide(msgs, &ids));
        return Some(Plan {
            mode: Mode::Elide,
            reason: Reason::Age,
            elide: ids,
            upto: None,
            request: None,
            tokens_before: before,
            tokens_after: after,
            reached: true,
        });
    }
    // Candidate A: stubs for every result outside the recent rounds; then
    // the same down to the newest round alone. Both are cheap and the model
    // can run a stubbed call again, so they come before a summary.
    let mut ids = elidable(msgs, &rounds, policy.keep_results);
    let mut elided = apply_elide(msgs, &ids);
    let mut after_a = size(&elided);
    if after_a > target && policy.keep_results > 1 {
        let all = elidable(msgs, &rounds, 1);
        if all.len() > ids.len() {
            elided = apply_elide(msgs, &all);
            after_a = size(&elided);
            ids = all;
        }
    }
    let plan_a = |reached| Plan {
        mode: Mode::Elide,
        reason,
        elide: ids.clone(),
        upto: None,
        request: None,
        tokens_before: before,
        tokens_after: after_a,
        reached,
    };
    if !ids.is_empty() && after_a <= target {
        return Some(plan_a(true));
    }
    // Candidate B: summarize everything before the last few rounds, keeping
    // as many of them as fit (at least one).
    let allowance = summary_allowance(budget);
    let first = first_unsummarized(&elided);
    let mut best: Option<(usize, u64)> = None;
    for keep in (1..=policy.keep_rounds.max(1)).rev() {
        let Some(&upto) = rounds.len().checked_sub(keep).and_then(|i| rounds.get(i)) else { continue };
        if upto <= first {
            continue;
        }
        let placeholder = Summary { upto, text: "x".repeat((allowance * 4) as usize), request: None };
        let after = size(&apply(elided.clone(), &Cut { elide: vec![], summary: Some(placeholder) }));
        best = Some((upto, after));
        if after <= target {
            break;
        }
    }
    match best {
        Some((upto, after)) if after < after_a || ids.is_empty() => Some(Plan {
            mode: Mode::Summarize,
            reason,
            elide: ids,
            upto: Some(upto),
            request: pinned_request(&elided, upto),
            tokens_before: before,
            tokens_after: after,
            reached: after <= target,
        }),
        _ if !ids.is_empty() => Some(plan_a(after_a <= target)),
        _ => None,
    }
}

/// Apply a journaled cut to the view. The agent and the projector both call
/// this, so a resumed session sees what the model saw.
pub fn apply(msgs: Vec<Message>, cut: &Cut) -> Vec<Message> {
    let mut msgs = apply_elide(&msgs, &cut.elide);
    if let Some(s) = &cut.summary
        && s.upto > 1
        && s.upto <= msgs.len()
    {
        let tail = msgs.split_off(s.upto);
        msgs.truncate(1);
        msgs.push(Message::user(summary_message(&s.text, s.request.as_deref())));
        if tail.first().is_some_and(|m| m.role == "user") {
            msgs.push(Message::text("assistant", FILLER));
        }
        msgs.extend(tail);
    }
    msgs
}

/// Whether an error from the endpoint says the prompt was too long.
pub fn is_overflow(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "maximum context length",
        "context window",
        "prompt is too long",
        "too many tokens",
        "exceeds the context",
        "input is too long",
        "reduce the length",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// The prompt size a provider reported in an overflow error ("resulted in
/// 40123 tokens"), when it says.
pub fn overflow_tokens(err: &str) -> Option<u64> {
    number_after(err, "resulted in ")
}

/// The window a provider stated in an overflow error ("maximum context
/// length is 32768 tokens"), when it says.
pub fn overflow_limit(err: &str) -> Option<u64> {
    number_after(err, "context length is ")
}

fn number_after(s: &str, prefix: &str) -> Option<u64> {
    let i = s.find(prefix)? + prefix.len();
    let digits: String = s[i..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Index of each assistant message: where rounds start.
fn rounds(msgs: &[Message]) -> Vec<usize> {
    msgs.iter().enumerate().filter(|(_, m)| m.role == "assistant").map(|(i, _)| i).collect()
}

/// Call ids of results, not yet stubbed and worth stubbing, older than the
/// last `keep` rounds.
fn elidable(msgs: &[Message], rounds: &[usize], keep: usize) -> Vec<String> {
    let Some(&edge) = rounds.len().checked_sub(keep).and_then(|i| rounds.get(i)) else { return vec![] };
    msgs[..edge]
        .iter()
        .filter(|m| m.role == "tool")
        .filter(|m| m.content.as_deref().is_some_and(|c| c.len() >= MIN_ELIDE && !c.starts_with(ELIDED)))
        .filter_map(|m| m.tool_call_id.clone())
        .collect()
}

fn apply_elide(msgs: &[Message], ids: &[String]) -> Vec<Message> {
    let ids: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    msgs.iter()
        .map(|m| match (&m.tool_call_id, &m.content) {
            (Some(id), Some(c)) if m.role == "tool" && ids.contains(id.as_str()) && !c.starts_with(ELIDED) => {
                Message { content: Some(stub(c)), ..m.clone() }
            }
            _ => m.clone(),
        })
        .collect()
}

/// A result's stand-in: its size and first line, so the model knows what it
/// had and can run the call again.
fn stub(out: &str) -> String {
    let first = out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    format!(
        "{ELIDED} {} lines, {} chars; began: {:?}. Run the call again if you need it.]",
        out.lines().count(),
        out.len(),
        crate::llm::truncate(first, 120)
    )
}

fn summary_message(text: &str, request: Option<&str>) -> String {
    let mut s = format!("{SUMMARY_HEAD}{}{SUMMARY_TAIL}", text.trim());
    if let Some(r) = request {
        s.push_str(REQUEST_HEAD);
        s.push_str(r);
    }
    s
}

fn is_summary(m: &Message) -> bool {
    m.role == "user" && m.content.as_deref().is_some_and(|c| c.starts_with(SUMMARY_HEAD))
}

/// The first index a new summary would have to cover: past the system prompt
/// and any previous summary and its filler.
fn first_unsummarized(msgs: &[Message]) -> usize {
    match msgs.get(1) {
        Some(m) if is_summary(m) => {
            if msgs.get(2).is_some_and(|m| m.role == "assistant" && m.content.as_deref() == Some(FILLER)) {
                3
            } else {
                2
            }
        }
        _ => 1,
    }
}

/// The latest user request, if it falls in `1..upto`, verbatim. A previous
/// summary carries its pinned request forward.
fn pinned_request(msgs: &[Message], upto: usize) -> Option<String> {
    let (i, m) = msgs.iter().enumerate().rev().find(|(_, m)| m.role == "user")?;
    if i >= upto {
        return None;
    }
    let c = m.content.clone().unwrap_or_default();
    if is_summary(m) { c.split_once(REQUEST_HEAD).map(|(_, r)| r.to_string()) } else { Some(c) }
}

#[cfg(test)]
mod tests;
