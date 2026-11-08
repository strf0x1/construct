//! What the model sees: the message list, its budget and the cuts that fit it
//! (ADR 0042). A `Context` strategy owns the view, appends every message the
//! agent records, and reacts to the threshold or an overflow; the agent keeps
//! the window, the request sizing, pricing and events.
//!
//! The one choice a reader must know: a strategy is selected by name from
//! `[context] strategy`, and the only one today is `compact`, which is the
//! ADR 0013 planner moved behind this trait unchanged.

mod compact;

use crate::config::Config;
use crate::event::Event;
use crate::llm::{self, Message, Usage};
use serde_json::Value;

pub const COMPACT: &str = "compact";
pub use crate::compact::Reason;

/// The strategies a config may name, in the order an error lists them.
pub const KNOWN: &[&str] = &[COMPACT];

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
    /// Observe a message the agent appended.
    fn push(&mut self, m: Message);
    /// Tool definitions this strategy contributes (none for `compact`).
    fn tools(&self) -> Vec<Value> {
        vec![]
    }
    /// React to the threshold before a request, or to a provider's overflow.
    async fn fit(&mut self, env: &Env<'_>, reason: Reason) -> Fit;
}

/// Build a strategy by name. Strategies hold no config, so replay can build
/// one without a `Config` (part 2, #196).
pub fn build(name: &str, messages: Vec<Message>) -> anyhow::Result<Box<dyn Context>> {
    match name {
        COMPACT => Ok(Box::new(compact::Compact::new(messages))),
        other => {
            anyhow::bail!(
                "context.strategy: unknown strategy {other:?} (expected {})",
                KNOWN.iter().map(|n| format!("{n:?}")).collect::<Vec<_>>().join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_names_the_only_strategy_and_lists_the_known_ones() {
        let default = crate::config::Config::default();
        assert_eq!(build(&default.context.strategy, vec![]).expect("the default strategy").name(), COMPACT);
        let message = match build("nope", vec![]) {
            Ok(_) => panic!("an unknown strategy must not build"),
            Err(e) => e.to_string(),
        };
        assert_eq!(message, r#"context.strategy: unknown strategy "nope" (expected "compact")"#);
    }
}
