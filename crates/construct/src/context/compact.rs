//! The `compact` strategy: the ADR 0013 planner behind `Context`, unchanged.
//! Old journals resume exactly as before, the `compacted` event is identical,
//! and a second strategy can land beside it without touching the loop.

use super::{COMPACT, Check, Context, Env, Fit};
use crate::compact::{self, Reason};
use crate::event::Event;
use crate::llm::{Message, Usage};
use std::time::Instant;

pub struct Compact {
    messages: Vec<Message>,
}

impl Compact {
    pub fn new(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    /// Summarize messages `1..upto` (past any earlier summary) with the
    /// summarizer model. Its input is capped to fit the same budget; its
    /// output limit leaves room for reasoning, and a summary cut off by that
    /// limit is refused rather than kept half-written.
    /// The usage is returned either way: a refused summary was still paid for.
    async fn summarize(&self, env: &Env<'_>, upto: usize, budget: u64) -> (Result<String, String>, Usage) {
        let max_chars = (budget as f64 * 0.6 * env.bytes_per_token) as usize;
        let words = compact::summary_allowance(budget) * 3 / 4;
        let req = vec![
            Message::system(format!("{}\nKeep it under {words} words.", compact::SUMMARY_SYSTEM)),
            Message::user(compact::summary_request(&self.messages[1..upto], max_chars)),
        ];
        let mut usage = Usage::default();
        // An empty answer is the model's flake (reasoning, then no text): one
        // more try is cheaper than losing the summary to a coarser cut.
        for _ in 0..2 {
            let model = &env.cfg.compaction.model;
            let (r, u) = env.llm.complete(model, req.clone(), Some(compact::summary_max_tokens(budget))).await;
            usage.add(&u);
            let c = match r {
                Ok(c) => c,
                Err(e) => return (Err(e), usage),
            };
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
}

#[async_trait::async_trait]
impl Context for Compact {
    fn name(&self) -> &'static str {
        COMPACT
    }

    fn messages(&self) -> &[Message] {
        &self.messages
    }

    fn push(&mut self, m: Message) {
        self.messages.push(m);
    }

    fn replay(&mut self, e: &Event, check: Check) -> anyhow::Result<()> {
        let Event::Compacted { before, after, cut, .. } = e else {
            return Ok(());
        };
        match check {
            Check::Lenient => {
                if let Some(cut) = cut {
                    self.messages = compact::apply(std::mem::take(&mut self.messages), cut);
                }
            }
            Check::Strict => {
                let Some(cut) = cut else {
                    anyhow::bail!("compaction has no replayable cut; history is incompatible with safe restoration");
                };
                anyhow::ensure!(*before == self.messages.len(), "compaction input does not match recorded history");
                if let Some(s) = &cut.summary {
                    anyhow::ensure!(s.upto > 1 && s.upto <= self.messages.len(), "invalid saved compaction boundary");
                }
                self.messages = compact::apply(std::mem::take(&mut self.messages), cut);
                anyhow::ensure!(*after == self.messages.len(), "compaction output does not match recorded history");
            }
        }
        Ok(())
    }

    async fn fit(&mut self, env: &Env<'_>, reason: Reason) -> Fit {
        let started = Instant::now();
        let budget = env.budget;
        let Some(plan) = compact::plan(&self.messages, env.size, budget, &env.cfg.compaction, reason) else {
            return if reason == Reason::Overflow { Fit::NothingToCut } else { Fit::Unchanged };
        };
        let (mut summary, mut fallback, mut usage) = (None, None, Usage::default());
        if let Some(upto) = plan.upto {
            let (r, u) = self.summarize(env, upto, budget).await;
            usage = u;
            match r {
                Ok(text) => summary = Some(text),
                Err(e) => fallback = Some(e),
            }
        }
        let cut = plan.cut(summary);
        if cut.summary.is_none() && cut.elide.is_empty() {
            return Fit::Failed(fallback.unwrap_or_default());
        }
        let before = self.messages.len();
        self.messages = compact::apply(std::mem::take(&mut self.messages), &cut);
        let tokens_after = (env.size)(&self.messages);
        Fit::Changed(Box::new(Event::Compacted {
            before,
            after: self.messages.len(),
            mode: if cut.summary.is_some() { "summarize" } else { "elide" }.into(),
            reason: plan.reason.label().into(),
            tokens_before: plan.tokens_before,
            tokens_after,
            budget,
            reached: tokens_after <= (env.cfg.compaction.target as f64 * budget as f64) as u64,
            ms: started.elapsed().as_millis() as u64,
            summary_prompt: usage.prompt_tokens,
            summary_completion: usage.completion_tokens,
            summary_cached: usage.cached_tokens,
            summary_cost: usage.cost,
            fallback,
            cut: Some(cut),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::apply;
    use crate::config::Config;
    use crate::context::Complete;
    use crate::llm::Completion;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        reply: Result<String, String>,
        calls: AtomicUsize,
    }

    impl Fake {
        fn new(reply: Result<&str, &str>) -> Self {
            Self { reply: reply.map(str::to_string).map_err(str::to_string), calls: AtomicUsize::new(0) }
        }
    }

    #[async_trait::async_trait]
    impl Complete for Fake {
        async fn complete(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _max_tokens: Option<u32>,
        ) -> (Result<Completion, String>, Usage) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.reply {
                Ok(text) => (
                    Ok(Completion {
                        message: Message::text("assistant", text.clone()),
                        usage: Usage::default(),
                        finish_reason: "stop".into(),
                    }),
                    Usage::default(),
                ),
                Err(e) => (Err(e.clone()), Usage::default()),
            }
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    fn size(messages: &[Message]) -> u64 {
        messages.iter().filter_map(|m| m.content.as_deref()).map(|c| c.len() as u64).sum()
    }

    /// Three rounds: an assistant answer, then its tool result.
    fn rounds(result_chars: &[usize], answer_chars: usize) -> Vec<Message> {
        let mut messages = vec![Message::system("system"), Message::user("request")];
        for (i, n) in result_chars.iter().enumerate() {
            messages.push(Message::text("assistant", "a".repeat(answer_chars)));
            messages.push(Message::tool(&format!("call-{i}"), "x".repeat(*n)));
        }
        messages
    }

    fn budget() -> u64 {
        1000
    }

    #[tokio::test]
    async fn big_old_results_elide_and_the_view_is_apply_of_the_recorded_cut() {
        let cfg = cfg();
        let fake = Fake::new(Ok("unused"));
        let mut strategy = Compact::new(rounds(&[2000, 2000, 2000], 10));
        let before = strategy.messages().to_vec();
        let fit = strategy
            .fit(&Env { size: &size, budget: budget(), bytes_per_token: 4.0, cfg: &cfg, llm: &fake }, Reason::Threshold)
            .await;
        let cut = match fit {
            Fit::Changed(event) => match *event {
                Event::Compacted { mode, cut: Some(cut), .. } => {
                    assert_eq!(mode, "elide");
                    cut
                }
                other => panic!("expected an elide cut, got {other:?}"),
            },
            other => panic!("expected an elide cut, got {other:?}"),
        };
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "eliding never calls the summarizer");
        assert_eq!(strategy.messages(), apply(before, &cut).as_slice());
    }

    #[tokio::test]
    async fn small_results_summarize_older_rounds() {
        let cfg = cfg();
        let fake = Fake::new(Ok("the summary"));
        let mut strategy = Compact::new(rounds(&[10, 10, 10], 2000));
        let before = strategy.messages().to_vec();
        let fit = strategy
            .fit(&Env { size: &size, budget: budget(), bytes_per_token: 4.0, cfg: &cfg, llm: &fake }, Reason::Threshold)
            .await;
        let cut = match fit {
            Fit::Changed(event) => match *event {
                Event::Compacted { mode, cut: Some(cut), .. } => {
                    assert_eq!(mode, "summarize");
                    cut
                }
                other => panic!("expected a summary cut, got {other:?}"),
            },
            other => panic!("expected a summary cut, got {other:?}"),
        };
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        assert_eq!(strategy.messages(), apply(before, &cut).as_slice());
        assert!(strategy.messages()[1].content.as_deref().is_some_and(|c| c.contains("the summary")));
    }

    #[tokio::test]
    async fn under_the_threshold_nothing_changes_and_nothing_is_asked() {
        let cfg = cfg();
        let fake = Fake::new(Ok("unused"));
        let messages = vec![Message::system("system"), Message::user("short")];
        let mut strategy = Compact::new(messages.clone());
        let fit = strategy
            .fit(&Env { size: &size, budget: 100_000, bytes_per_token: 4.0, cfg: &cfg, llm: &fake }, Reason::Threshold)
            .await;
        assert!(matches!(fit, Fit::Unchanged), "{fit:?}");
        assert_eq!(strategy.messages(), messages.as_slice());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_overflow_with_nothing_to_cut_says_so() {
        let cfg = cfg();
        let fake = Fake::new(Ok("unused"));
        let mut strategy = Compact::new(vec![Message::system("system"), Message::user("short")]);
        let fit = strategy
            .fit(&Env { size: &size, budget: budget(), bytes_per_token: 4.0, cfg: &cfg, llm: &fake }, Reason::Overflow)
            .await;
        assert!(matches!(fit, Fit::NothingToCut), "{fit:?}");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }

    fn compacted(before: usize, after: usize, cut: Option<compact::Cut>) -> Event {
        serde_json::from_value(serde_json::json!({
            "event": "compacted",
            "before": before,
            "after": after,
            "cut": cut,
        }))
        .expect("a compacted event")
    }

    #[test]
    fn replay_applies_each_cut_where_it_happened() {
        let first = vec![
            Message::system("system"),
            Message::user("one"),
            Message::text("assistant", "answer one"),
            Message::tool("call-1", "x".repeat(700)),
        ];
        let elide = compact::Cut { elide: vec!["call-1".into()], summary: None };
        let after_elide = apply(first.clone(), &elide);
        let summarize = compact::Cut {
            elide: vec![],
            summary: Some(compact::Summary { upto: 3, text: "earlier rounds".into(), request: None }),
        };
        let mut middle = after_elide.clone();
        middle.push(Message::user("two"));
        middle.push(Message::text("assistant", "answer two"));
        let expected = apply(middle.clone(), &summarize);

        let events = [
            Event::for_message(Message::system("system")),
            Event::for_message(Message::user("one")),
            Event::for_message(Message::text("assistant", "answer one")),
            Event::for_message(Message::tool("call-1", "x".repeat(700))),
            compacted(first.len(), after_elide.len(), Some(elide)),
            Event::for_message(Message::user("two")),
            Event::for_message(Message::text("assistant", "answer two")),
            compacted(middle.len(), expected.len(), Some(summarize)),
        ];
        let ctx = crate::context::replay(events.iter(), Check::Lenient).expect("a lenient replay");
        assert_eq!(ctx.messages(), expected.as_slice());
    }

    #[test]
    fn strict_replay_refuses_a_cut_less_compaction_and_lenient_skips_it() {
        let events = [
            Event::for_message(Message::system("system")),
            compacted(1, 1, None),
            Event::for_message(Message::user("hi")),
        ];
        let ctx = crate::context::replay(events.iter(), Check::Lenient).expect("a lenient replay");
        let expected = vec![Message::system("system"), Message::user("hi")];
        assert_eq!(ctx.messages(), expected.as_slice());
        let err = match crate::context::replay(events.iter(), Check::Strict) {
            Ok(_) => panic!("strict must refuse a cut-less compaction"),
            Err(e) => e.to_string(),
        };
        assert_eq!(err, "compaction has no replayable cut; history is incompatible with safe restoration");
    }

    #[test]
    fn strict_replay_refuses_a_cut_that_does_not_match_the_history() {
        let events = [
            Event::for_message(Message::system("system")),
            compacted(7, 1, Some(compact::Cut { elide: vec![], summary: None })),
        ];
        let err = match crate::context::replay(events.iter(), Check::Strict) {
            Ok(_) => panic!("strict must refuse a mismatched compaction"),
            Err(e) => e.to_string(),
        };
        assert_eq!(err, "compaction input does not match recorded history");
    }

    #[test]
    fn strategy_of_treats_an_empty_head_as_compact() {
        let manifest = crate::manifest::Manifest::foreign(&cfg());
        let head = Event::Head {
            id: "s".into(),
            harness: "construct".into(),
            version: "test".into(),
            cwd: "/".into(),
            model: "m".into(),
            manifest,
            parent_session: None,
            parent_call_id: None,
        };
        assert_eq!(crate::context::strategy_of([&head]), COMPACT);
        assert_eq!(crate::context::strategy_of(std::iter::empty()), COMPACT);
    }

    #[tokio::test]
    async fn a_failed_summarizer_with_nothing_to_stub_is_a_failure() {
        let cfg = cfg();
        let fake = Fake::new(Err("the provider said no"));
        let mut strategy = Compact::new(rounds(&[10, 10, 10], 2000));
        let fit = strategy
            .fit(&Env { size: &size, budget: budget(), bytes_per_token: 4.0, cfg: &cfg, llm: &fake }, Reason::Threshold)
            .await;
        match fit {
            Fit::Failed(detail) => assert_eq!(detail, "the provider said no"),
            other => panic!("expected a failure, got {other:?}"),
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }
}
