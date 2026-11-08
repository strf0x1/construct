//! What the model sees: the message list, its budget and the cuts that fit it
//! (ADR 0042). A `Context` strategy owns the view, appends every message the
//! agent records, and reacts to the threshold or an overflow; the agent keeps
//! the window, the request sizing, pricing and events.
//!
//! The one choice a reader must know: a strategy is selected by name from
//! `[context] strategy`. `compact` (the ADR 0013 planner) is the default;
//! `optchat` (ADR 0043) keeps every message as a tree of short lines.

mod compact;
mod optchat;

use crate::config::Config;
use crate::event::Event;
use crate::llm::{self, Message, Usage};
use serde_json::Value;

pub const COMPACT: &str = "compact";
pub const OPTCHAT: &str = "optchat";
pub use crate::compact::Reason;

/// The strategies a config may name, in the order an error lists them.
pub const KNOWN: &[&str] = &[COMPACT, OPTCHAT];

/// How strictly a journal is replayed. Lenient resume skips a `compacted`
/// event from before `cut` was journaled; recovery refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Lenient,
    Strict,
}

/// The outcome of one fit.
#[derive(Debug)]
pub enum Fit {
    /// Nothing changed; the prompt still fits.
    Unchanged,
    /// The view changed, and this event records it for the journal.
    Changed(Box<Event>),
    /// A cut was forced (an overflow) and nothing was left to cut.
    NothingToCut,
    /// A cut was planned but produced nothing; the reason is prose.
    Failed(String),
}

/// What a strategy may use for one fit. The agent builds it per call so a
/// strategy never holds a client, a config or the live view's size closure.
pub struct Env<'a> {
    /// Estimated prompt tokens for a candidate message list, tools included.
    pub size: &'a (dyn Fn(&[Message]) -> u64 + Send + Sync),
    pub budget: u64,
    pub bytes_per_token: f64,
    pub cfg: &'a Config,
    pub llm: &'a dyn Complete,
}

/// One tool-less completion, for a strategy's own model calls. Usage is
/// returned priced, even on error.
#[async_trait::async_trait]
pub trait Complete: Send + Sync {
    /// `model` empty means the run's current model.
    async fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        max_tokens: Option<u32>,
    ) -> (Result<llm::Completion, String>, Usage);
}

/// The message list the model is sent, and the policy that keeps it in budget.
#[async_trait::async_trait]
pub trait Context: Send + Sync {
    fn name(&self) -> &'static str;
    /// The view sent to the model.
    fn messages(&self) -> &[Message];
    /// Every message the log holds, including the ones the view no longer
    /// shows. Readers that need what actually happened (learning, routing)
    /// read this, not `messages()` (ADR 0043 decision 12).
    fn log(&self) -> &[Message] {
        self.messages()
    }
    /// Observe a message the agent appended.
    fn push(&mut self, m: Message);
    /// Swap in the session's current system prompt. Replaces the recorded one
    /// if there was one, inserts one otherwise, and returns whether one was
    /// recorded. Resume keeps the replayed strategy, so this is how the
    /// prompt is refreshed without losing the strategy's own state.
    fn set_system(&mut self, m: Message) -> bool;
    /// Replay a journal event that is not message-bearing, in order.
    fn replay(&mut self, e: &Event, check: Check) -> anyhow::Result<()>;
    /// Tool definitions this strategy contributes (none for `compact`,
    /// `optchat`'s `zoom`).
    fn tools(&self) -> Vec<Value> {
        vec![]
    }
    /// Answer a call to one of `tools()`. `None` means the name is not one of
    /// this strategy's tools; `Some(Ok(text))` is the model's result and
    /// `Some(Err(text))` a refusal, which the journal records as not ok.
    fn call(&mut self, _name: &str, _args: &str) -> Option<Result<String, String>> {
        None
    }
    /// React to the threshold before a request, or to a provider's overflow.
    async fn fit(&mut self, env: &Env<'_>, reason: Reason) -> Fit;
}

/// Build a strategy by name. Strategies hold no config, so replay can build
/// one without a `Config` (part 2, #196).
pub fn build(name: &str, messages: Vec<Message>) -> anyhow::Result<Box<dyn Context>> {
    match name {
        COMPACT => Ok(Box::new(compact::Compact::new(messages))),
        OPTCHAT => Ok(Box::new(optchat::Optchat::new(messages))),
        other => {
            anyhow::bail!(
                "context.strategy: unknown strategy {other:?} (expected {})",
                KNOWN.iter().map(|n| format!("{n:?}")).collect::<Vec<_>>().join(", ")
            )
        }
    }
}

/// The strategy a session's head names. A journal from before the manifest
/// carried one, or a session with no head at all, is `compact`.
pub fn strategy_of<'a>(events: impl IntoIterator<Item = &'a Event>) -> &'a str {
    events
        .into_iter()
        .find_map(|e| match e {
            Event::Head { manifest, .. } => Some(manifest.context.as_str()),
            _ => None,
        })
        .map(|name| if name.is_empty() { COMPACT } else { name })
        .unwrap_or(COMPACT)
}

/// Rebuild a session's view through the strategy its head names, replaying
/// every non-message event in order.
pub fn replay<'a, I>(events: I, check: Check) -> anyhow::Result<Box<dyn Context>>
where
    I: IntoIterator<Item = &'a Event> + Clone,
{
    let mut ctx = build(strategy_of(events.clone()), vec![])?;
    for e in events {
        if let Some(m) = e.message() {
            ctx.push(m.clone());
        } else {
            ctx.replay(e, check)?;
        }
    }
    Ok(ctx)
}

/// Refuse to replay a session whose head names another strategy. `recorded`
/// is empty for journals from before the manifest carried one.
pub fn check_resume(recorded: &str, configured: &str) -> anyhow::Result<()> {
    let recorded = if recorded.is_empty() { COMPACT } else { recorded };
    if recorded != configured {
        anyhow::bail!(
            "session uses context strategy {recorded:?} but config names {configured:?}; resume with CONSTRUCT_CONTEXT_STRATEGY={recorded}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_names_the_known_strategies_and_lists_them() {
        let default = crate::config::Config::default();
        assert_eq!(build(&default.context.strategy, vec![]).expect("the default strategy").name(), COMPACT);
        assert_eq!(build(OPTCHAT, vec![]).expect("optchat").name(), OPTCHAT);
        let message = match build("nope", vec![]) {
            Ok(_) => panic!("an unknown strategy must not build"),
            Err(e) => e.to_string(),
        };
        assert_eq!(message, r#"context.strategy: unknown strategy "nope" (expected "compact", "optchat")"#);
    }

    #[test]
    fn only_optchat_offers_tools() {
        assert!(build(COMPACT, vec![]).expect("compact").tools().is_empty());
        assert_eq!(build(OPTCHAT, vec![]).expect("optchat").tools().len(), 1);
    }

    #[test]
    fn check_resume_accepts_an_unrecorded_or_matching_strategy_and_refuses_another() {
        check_resume("", COMPACT).expect("an old journal is compact");
        check_resume(COMPACT, COMPACT).expect("the same strategy");
        let message = check_resume("optchat", COMPACT).expect_err("a different strategy must refuse").to_string();
        assert_eq!(
            message,
            r#"session uses context strategy "optchat" but config names "compact"; resume with CONSTRUCT_CONTEXT_STRATEGY=optchat"#
        );
    }
}
