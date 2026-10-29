//! What a session has spent, folded from its events: the `/status` block,
//! `sessions show` and the TUI's status line all add up the same way.
//!
//! The design choice: one `add` per event, so the live screen (fed `Event`s
//! as they happen) and a reader of the journal (fed rows) cannot disagree.
//! Everything billed counts: the loop's own requests, compaction summaries,
//! and delegated children (a live `Child` wrapper, or the child's session in
//! the journal). Only the loop's own requests say how full the window is.

use super::{Entry, Journal};
use crate::event::Event;
use crate::llm::Usage;
use anyhow::Result;

#[derive(Debug, Default, Clone)]
pub struct Totals {
    /// Model calls billed, side calls included.
    pub requests: u64,
    /// Everything billed.
    pub usage: Usage,
    /// The part of `usage` spent by summaries and sub-agents.
    pub side: Usage,
    /// The newest own request's prompt size: how full the window is now.
    pub last_prompt: u64,
}

impl Totals {
    /// Fold in one event. Anything that bills nothing is ignored.
    pub fn add(&mut self, ev: &Event) {
        match ev {
            Event::Usage { prompt, completion, cached, cache_write, cost } => {
                self.requests += 1;
                self.last_prompt = *prompt;
                self.usage.add(&Usage {
                    prompt_tokens: *prompt,
                    completion_tokens: *completion,
                    cached_tokens: *cached,
                    cache_write_tokens: *cache_write,
                    cost: *cost,
                });
            }
            Event::Compacted { summary_prompt, summary_completion, summary_cached, summary_cost, .. }
                if summary_prompt + summary_completion > 0 =>
            {
                self.side_usage(
                    1,
                    &Usage {
                        prompt_tokens: *summary_prompt,
                        completion_tokens: *summary_completion,
                        cached_tokens: *summary_cached,
                        cache_write_tokens: 0,
                        cost: *summary_cost,
                    },
                );
            }
            Event::Child { inner, .. } => {
                let mut child = Totals::default();
                child.add(inner);
                self.side_usage(child.requests, &child.usage);
            }
            _ => {}
        }
    }

    fn side_usage(&mut self, requests: u64, u: &Usage) {
        self.requests += requests;
        self.usage.add(u);
        self.side.add(u);
    }

    /// One session's rows, without its children.
    pub fn of(entries: &[Entry]) -> Self {
        let mut t = Self::default();
        for e in entries {
            t.add(&e.event);
        }
        t
    }

    /// A session as `/status` reports it: its own rows, plus every session
    /// it delegated to (named in the `delegate` result's `data.session`).
    pub fn session(db: &Journal, entries: &[Entry]) -> Result<Self> {
        let mut t = Self::of(entries);
        t.children(db, entries, crate::child::MAX_DEPTH)?;
        Ok(t)
    }

    fn children(&mut self, db: &Journal, entries: &[Entry], depth: u32) -> Result<()> {
        if depth == 0 {
            return Ok(());
        }
        for e in entries {
            let Event::ToolEnd { data: Some(data), .. } = &e.event else { continue };
            let Some(id) = data.get("session").and_then(|v| v.as_str()) else { continue };
            let rows = db.since(id, 0)?;
            let mut child = Self::of(&rows);
            child.children(db, &rows, depth - 1)?;
            self.side_usage(child.requests, &child.usage);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u64, cost: f64) -> Event {
        Event::Usage { prompt, completion: 10, cached: prompt / 2, cache_write: 0, cost: Some(cost) }
    }

    #[test]
    fn side_calls_count_but_do_not_move_the_window() {
        let mut t = Totals::default();
        t.add(&usage(1000, 0.01));
        t.add(&Event::Child { id: "call-1".into(), inner: Box::new(usage(5000, 0.05)) });
        let compacted: Event = serde_json::from_value(serde_json::json!({
            "event": "compacted", "before": 9, "after": 3,
            "summary_prompt": 800, "summary_completion": 100, "summary_cost": 0.002,
        }))
        .unwrap();
        t.add(&compacted);
        assert_eq!(t.requests, 3);
        assert_eq!(t.last_prompt, 1000, "only the loop's own requests fill the window");
        assert_eq!((t.usage.prompt_tokens, t.usage.completion_tokens), (6800, 120));
        assert!((t.usage.cost.unwrap() - 0.062).abs() < 1e-9);
        assert!((t.side.cost.unwrap() - 0.052).abs() < 1e-9);
    }

    #[test]
    fn a_compaction_without_a_summary_bills_nothing() {
        let mut t = Totals::default();
        let elided: Event =
            serde_json::from_value(serde_json::json!({ "event": "compacted", "before": 9, "after": 3 })).unwrap();
        t.add(&elided);
        assert_eq!((t.requests, t.usage.cost), (0, None));
    }
}
