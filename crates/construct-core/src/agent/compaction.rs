//! The agent's side of fitting: ask the context strategy to cut, and turn its
//! answer into the event or note a run already emitted.
//!
//! The one choice a reader must know: only an overflow makes a failed cut an
//! `Error`. Otherwise the prompt still fits, the turn goes on uncut, and the
//! failure is a `Note` (ADR 0025), so a run that recovered does not fail.

use super::{Agent, Event, Events};
use crate::compact::{self, Reason};
use crate::config::Config;
use crate::context::{Complete, Env, Fit};
use crate::journal::notes::Kind;
use crate::llm::{self, Message, Usage};
use serde_json::Value;

/// The summarizer failed and there was nothing to stub, below the window.
const SUMMARY_FAILED: Kind = Kind { class: "compaction", name: "summary_failed", cap: 20 };

impl Agent {
    /// The prompt budget: the window as known now, less the output reserve.
    pub(super) fn budget(&self) -> u64 {
        compact::budget(self.cfg_context_window(), self.cfg.max_tokens)
    }

    /// Let the strategy cut history if it says so. Returns whether anything
    /// was cut.
    pub(super) async fn compact(&mut self, defs: &[Value], reason: Reason, tx: &Events) -> bool {
        let budget = self.budget();
        let ratio = self.bytes_per_token;
        let (client, model, max_tokens, temperature) =
            (self.client.clone(), self.cfg.wire_id(&self.cfg.model), self.cfg.max_tokens, self.cfg.temperature);
        let size = move |messages: &[Message]| {
            let req = llm::Request { model: &model, messages, tools: defs, max_tokens, temperature };
            (client.body(&req).len() as f64 / ratio) as u64
        };
        let llm = Llm { client: &self.client, cfg: &self.cfg };
        let fit = {
            let env = Env { size: &size, budget, bytes_per_token: ratio, cfg: &self.cfg, llm: &llm };
            self.context.fit(&env, reason).await
        };
        match fit {
            Fit::Changed(event) => {
                self.journal.emit(*event, tx);
                self.last_prompt_tokens = 0;
                true
            }
            Fit::Unchanged => false,
            Fit::NothingToCut => {
                if reason == Reason::Overflow {
                    self.journal
                        .emit(Event::Error { message: "the prompt is too long and nothing is left to cut".into() }, tx);
                }
                false
            }
            Fit::Failed(detail) => {
                if reason == Reason::Overflow {
                    self.journal.emit(Event::Error { message: format!("compaction failed: {detail}") }, tx);
                } else {
                    self.notes.record(SUMMARY_FAILED, &detail);
                    self.notes.flush(&self.journal, tx);
                }
                false
            }
        }
    }
}

/// The strategy's one model call, priced from the catalog like any request.
struct Llm<'a> {
    client: &'a llm::Client,
    cfg: &'a Config,
}

#[async_trait::async_trait]
impl Complete for Llm<'_> {
    async fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        max_tokens: Option<u32>,
    ) -> (Result<llm::Completion, String>, Usage) {
        let selector = if model.is_empty() { &self.cfg.model } else { model };
        let (client, wire) = match self.client.for_model(self.cfg, selector) {
            Ok(v) => v,
            Err(e) => return (Err(format!("{e:#}")), Usage::default()),
        };
        let req = llm::Request { model: &wire, messages: &messages, tools: &[], max_tokens, temperature: None };
        match client.complete(req).await {
            Ok(mut c) => {
                super::limits::price(self.cfg, selector, &mut c.usage);
                let usage = c.usage.clone();
                (Ok(c), usage)
            }
            Err(e) => (Err(format!("{e:#}")), Usage::default()),
        }
    }
}
