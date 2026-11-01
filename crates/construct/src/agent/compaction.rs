//! The agent's side of compaction: ask the planner for a cut, have the
//! summarizer write the summary, apply the cut and journal it.
//!
//! The one choice a reader must know: only an overflow makes a failed cut an
//! `Error`. Otherwise the prompt still fits, the turn goes on uncut, and the
//! failure is a `Note` (ADR 0025), so a run that recovered does not fail.

use super::{Agent, Event, Events};
use crate::compact::{self, Reason};
use crate::journal::notes::Kind;
use crate::llm::{self, Message, Usage};
use serde_json::Value;
use std::time::Instant;

/// The summarizer failed and there was nothing to stub, below the window.
const SUMMARY_FAILED: Kind = Kind { class: "compaction", name: "summary_failed", cap: 20 };

impl Agent {
    /// The prompt budget: the window as known now, less the output reserve.
    pub(super) fn budget(&self) -> u64 {
        compact::budget(self.cfg_context_window(), self.cfg.max_tokens)
    }

    /// Cut history if the planner says so (see `compact`). Returns whether
    /// anything was cut.
    pub(super) async fn compact(&mut self, defs: &[Value], reason: Reason, tx: &Events) -> bool {
        let started = Instant::now();
        let budget = self.budget();
        let ratio = self.bytes_per_token;
        let (client, model, max_tokens, temperature) =
            (self.client.clone(), self.cfg.wire_id(&self.cfg.model), self.cfg.max_tokens, self.cfg.temperature);
        let size = move |messages: &[Message]| {
            let req = llm::Request { model: &model, messages, tools: defs, max_tokens, temperature };
            (client.body(&req).len() as f64 / ratio) as u64
        };
        let Some(plan) = compact::plan(&self.messages, &size, budget, &self.cfg.compaction, reason) else {
            if reason == Reason::Overflow {
                self.journal
                    .emit(Event::Error { message: "the prompt is too long and nothing is left to cut".into() }, tx);
            }
            return false;
        };
        let (mut summary, mut fallback, mut usage) = (None, None, Usage::default());
        if let Some(upto) = plan.upto {
            let (r, mut u) = self.summarize(upto, budget).await;
            self.price(self.summarizer(), &mut u);
            usage = u;
            match r {
                Ok(text) => summary = Some(text),
                Err(e) => fallback = Some(e),
            }
        }
        let cut = plan.cut(summary);
        if cut.summary.is_none() && cut.elide.is_empty() {
            let detail = fallback.unwrap_or_default();
            if reason == Reason::Overflow {
                self.journal.emit(Event::Error { message: format!("compaction failed: {detail}") }, tx);
            } else {
                self.notes.record(SUMMARY_FAILED, &detail);
                self.notes.flush(&self.journal, tx);
            }
            return false;
        }
        let before = self.messages.len();
        self.messages = compact::apply(std::mem::take(&mut self.messages), &cut);
        let tokens_after = size(&self.messages);
        self.last_prompt_tokens = 0;
        self.journal.emit(
            Event::Compacted {
                before,
                after: self.messages.len(),
                mode: if cut.summary.is_some() { "summarize" } else { "elide" }.into(),
                reason: plan.reason.label().into(),
                tokens_before: plan.tokens_before,
                tokens_after,
                budget,
                reached: tokens_after <= (self.cfg.compaction.target as f64 * budget as f64) as u64,
                ms: started.elapsed().as_millis() as u64,
                summary_prompt: usage.prompt_tokens,
                summary_completion: usage.completion_tokens,
                summary_cached: usage.cached_tokens,
                summary_cost: usage.cost,
                fallback,
                cut: Some(cut),
            },
            tx,
        );
        true
    }

    /// Summarize messages `1..upto` (past any earlier summary) with the
    /// summarizer model. Its input is capped to fit the same budget; its
    /// output limit leaves room for reasoning, and a summary cut off by that
    /// limit is refused rather than kept half-written.
    /// The usage is returned either way: a refused summary was still paid for.
    async fn summarize(&self, upto: usize, budget: u64) -> (Result<String, String>, Usage) {
        let max_chars = (budget as f64 * 0.6 * self.bytes_per_token) as usize;
        let words = compact::summary_allowance(budget) * 3 / 4;
        let req = vec![
            Message::system(format!("{}\nKeep it under {words} words.", compact::SUMMARY_SYSTEM)),
            Message::user(compact::summary_request(&self.messages[1..upto], max_chars)),
        ];
        let mut usage = Usage::default();
        let (client, model) = match self.client.for_model(&self.cfg, self.summarizer()) {
            Ok(c) => c,
            Err(e) => return (Err(format!("{e:#}")), usage),
        };
        // An empty answer is the model's flake (reasoning, then no text): one
        // more try is cheaper than losing the summary to a coarser cut.
        for _ in 0..2 {
            let r = client
                .complete(llm::Request {
                    model: &model,
                    messages: &req,
                    tools: &[],
                    max_tokens: Some(compact::summary_max_tokens(budget)),
                    temperature: None,
                })
                .await;
            let c = match r {
                Ok(c) => c,
                Err(e) => return (Err(format!("{e:#}")), usage),
            };
            usage.add(&c.usage);
            let text = c.message.content.unwrap_or_default();
            if c.finish_reason == "length" {
                return (Err(format!("the summary hit the output limit after {} chars", text.len())), usage);
            }
            if !text.trim().is_empty() {
                return (Ok(text), usage);
            }
        }
        (Err("the summarizer returned nothing, twice".into()), usage)
    }

    /// The model that writes compaction summaries.
    fn summarizer(&self) -> &str {
        if self.cfg.compaction.model.is_empty() { &self.cfg.model } else { &self.cfg.compaction.model }
    }
}
