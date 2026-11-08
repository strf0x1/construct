//! `fit` for `optchat` (ADR 0043 decisions 7, 10 and 11): write the leaves a
//! turn needs, merge built parents, batch the view down under `high`, and fold
//! a turn's older rounds when the request is too big.
//!
//! The one choice a reader must know: every fit waits for its own summary
//! calls and journals them in one `Event::Memory`; a failed call still leaves
//! a fallback line, so the view never has a hole and `NothingToCut` is only
//! ever about a fold.

use super::summary::{self, Summary};
use super::tree::entry_text;
use super::view::turn_of;
use super::{Env, Fit, Optchat};
use crate::compact::Reason;
use crate::context::Context;
use crate::event::{Event, MemoryNode};
use crate::llm::Usage;
use futures::{StreamExt, stream};
use std::collections::BTreeSet;
use std::time::Instant;

impl Optchat {
    /// The body of `Context::fit`. See the module doc for the order.
    pub(super) async fn fitting(&mut self, env: &Env<'_>, reason: Reason) -> Fit {
        let started = Instant::now();
        let mut rec = Record::default();
        let turn = turn_of(&self.entries);

        if reason == Reason::Threshold && turn > self.view.frontier {
            self.write_leaves(env, self.view.frontier, turn, &mut rec).await;
            self.view.advance(turn);
            rec.moved = true;
            self.refresh();
            let limit = env.cfg.context.optchat.concurrency.max(1);
            self.write_parents(env, limit, &mut rec).await;
            if self.view_bytes() > env.cfg.context.optchat.high {
                self.batch(env, &mut rec).await;
            }
        }

        let threshold = f64::from(env.cfg.compaction.threshold) * env.budget as f64;
        let over = reason == Reason::Overflow || (env.size)(self.messages()) as f64 > threshold;
        if over && !self.fold(env, &mut rec).await && reason == Reason::Overflow {
            return Fit::NothingToCut;
        }

        if !rec.changed() {
            return if reason == Reason::Overflow { Fit::NothingToCut } else { Fit::Unchanged };
        }
        self.refresh();
        Fit::Changed(Box::new(Event::Memory {
            leaves: rec.leaves,
            merges: rec.merges,
            view: rec.moved.then(|| self.view.nodes.clone()),
            summary_prompt: rec.usage.prompt_tokens,
            summary_completion: rec.usage.completion_tokens,
            ms: started.elapsed().as_millis() as u64,
        }))
    }

    /// The view's bytes: each line plus its newline.
    fn view_bytes(&self) -> u64 {
        self.view
            .nodes
            .iter()
            .filter_map(|&(l, i)| self.tree.line(&self.entries, l, i))
            .map(|l| l.len() as u64 + 1)
            .sum()
    }

    /// Write a leaf for every missing line in `[from, to)`, bounded by
    /// `concurrency`, then insert them in index order.
    async fn write_leaves(&mut self, env: &Env<'_>, from: usize, to: usize, rec: &mut Record) {
        let missing: Vec<(usize, String)> = (from..to)
            .filter(|&i| !self.tree.built(&self.entries, 0, i as u64))
            .filter_map(|i| self.entries.get(i).map(|m| (i, entry_text(m))))
            .collect();
        if missing.is_empty() {
            return;
        }
        let concurrency = env.cfg.context.optchat.concurrency.max(1);
        let mut done: Vec<(usize, Summary)> = stream::iter(missing)
            .map(|(i, text)| async move { (i, summary::leaf(env, text).await) })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        done.sort_by_key(|(i, _)| *i);
        for (i, s) in done {
            self.tree.insert(0, i as u64, s.text.clone(), s.fallback);
            rec.usage.add(&s.usage);
            rec.leaves.push(MemoryNode { l: 0, i: i as u64, text: s.text, fallback: s.fallback });
        }
    }

    /// Write parents for the unbuilt adjacent sibling pairs in the view,
    /// oldest first, at most `limit`. Returns how many were written.
    async fn write_parents(&mut self, env: &Env<'_>, limit: usize, rec: &mut Record) -> usize {
        let mut seen = BTreeSet::new();
        let mut pairs: Vec<(u128, u32, u64, String, String)> = Vec::new();
        for w in self.view.nodes.windows(2) {
            let ((l, i), (l2, i2)) = (w[0], w[1]);
            if l2 != l || i2 != i + 1 || i % 2 != 0 {
                continue;
            }
            let j = i / 2;
            if self.tree.built(&self.entries, l + 1, j) || !seen.insert((l + 1, j)) {
                continue;
            }
            let (Some(first), Some(second)) =
                (self.tree.line(&self.entries, l, i), self.tree.line(&self.entries, l, i + 1))
            else {
                continue;
            };
            pairs.push((due(l, i2), l + 1, j, first, second));
        }
        pairs.sort_by_key(|p| p.0);
        pairs.truncate(limit);
        if pairs.is_empty() {
            return 0;
        }
        let concurrency = env.cfg.context.optchat.concurrency.max(1);
        let mut done: Vec<(u32, u64, Summary)> = stream::iter(pairs)
            .map(|(_, l, j, first, second)| async move { (l, j, summary::merge(env, first, second).await) })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        done.sort_by_key(|(l, j, _)| (*l, *j));
        let count = done.len();
        for (l, j, s) in done {
            self.tree.insert(l, j, s.text.clone(), s.fallback);
            rec.usage.add(&s.usage);
            rec.merges.push(MemoryNode { l, i: j, text: s.text, fallback: s.fallback });
        }
        count
    }

    /// Coarsen the view while it is over `high`, until it is at or below half.
    async fn batch(&mut self, env: &Env<'_>, rec: &mut Record) {
        let high = env.cfg.context.optchat.high;
        let limit = env.cfg.context.optchat.concurrency.max(1);
        if self.view_bytes() <= high {
            return;
        }
        loop {
            match self.most_due_built_pair() {
                Some((k, l, j)) => {
                    self.view.nodes[k] = (l, j);
                    self.view.nodes.remove(k + 1);
                    rec.moved = true;
                    if self.view_bytes() <= high / 2 {
                        return;
                    }
                }
                None => {
                    if self.write_parents(env, limit, rec).await == 0 {
                        return;
                    }
                }
            }
        }
    }

    /// The most due adjacent sibling pair whose parent is built: the pair
    /// with the smallest last entry index.
    fn most_due_built_pair(&self) -> Option<(usize, u32, u64)> {
        let mut best: Option<(u128, usize, u32, u64)> = None;
        for (k, w) in self.view.nodes.windows(2).enumerate() {
            let ((l, i), (l2, i2)) = (w[0], w[1]);
            if l2 != l || i2 != i + 1 || i % 2 != 0 {
                continue;
            }
            let j = i / 2;
            if !self.tree.built(&self.entries, l + 1, j) {
                continue;
            }
            let key = due(l, i2);
            if best.as_ref().is_none_or(|b| key < b.0) {
                best = Some((key, k, l + 1, j));
            }
        }
        best.map(|(_, k, l, j)| (k, l, j))
    }

    /// Fold the turn's raw rounds, all but the newest. A round starts at an
    /// assistant message, so a result is never separated from its call.
    /// Returns whether anything was folded.
    async fn fold(&mut self, env: &Env<'_>, rec: &mut Record) -> bool {
        let turn = turn_of(&self.entries);
        let raw_start = (turn + 1).max(self.view.frontier);
        let Some(last) = (raw_start..self.entries.len()).rev().find(|&i| self.entries[i].role == "assistant") else {
            return false;
        };
        if last <= raw_start {
            return false;
        }
        self.write_leaves(env, self.view.frontier.min(last), last, rec).await;
        self.view.advance(last);
        rec.moved = true;
        true
    }
}

/// A pair's age: the index of its last entry, `(2j+2)·2^l − 1`.
fn due(l: u32, last: u64) -> u128 {
    (u128::from(last) + 1) * (1u128 << l) - 1
}

/// Everything one fit wrote, before it becomes an `Event::Memory`.
#[derive(Default)]
struct Record {
    leaves: Vec<MemoryNode>,
    merges: Vec<MemoryNode>,
    usage: Usage,
    moved: bool,
}

impl Record {
    fn changed(&self) -> bool {
        self.moved || !self.leaves.is_empty() || !self.merges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Check, Complete, Env, Fit};
    use crate::llm::{Completion, FunctionCall, Message, ToolCall};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Answers every call with `reply`, tracks how many run at once, and can
    /// fail one chosen call.
    struct Fake {
        reply: String,
        fail_at: Option<usize>,
        calls: AtomicUsize,
        inflight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Fake {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.into(),
                fail_at: None,
                calls: AtomicUsize::new(0),
                inflight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Complete for Fake {
        async fn complete(&self, _: &str, _: Vec<Message>, _: Option<u32>) -> (Result<Completion, String>, Usage) {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            if self.fail_at == Some(n) {
                return (Err("the provider said no".into()), Usage::default());
            }
            (
                Ok(Completion {
                    message: Message::text("assistant", self.reply.clone()),
                    usage: Usage::default(),
                    finish_reason: "stop".into(),
                }),
                Usage::default(),
            )
        }
    }

    fn cfg() -> crate::config::Config {
        crate::config::Config::default()
    }

    fn size(messages: &[Message]) -> u64 {
        messages
            .iter()
            .map(|m| {
                m.content.as_deref().map_or(0, |c| c.len() as u64)
                    + m.parts.iter().map(|p| p.text.len() as u64).sum::<u64>()
            })
            .sum()
    }

    fn env<'a>(
        cfg: &'a crate::config::Config,
        fake: &'a Fake,
        size: &'a (dyn Fn(&[Message]) -> u64 + Send + Sync),
    ) -> Env<'a> {
        Env { size, budget: 1000, bytes_per_token: 4.0, cfg, llm: fake }
    }

    fn long(role: &str, n: usize) -> Message {
        Message::text(role, "x".repeat(n))
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall { name: "read".into(), arguments: "{}".into() },
        }
    }

    fn assistant_with(call_id: &str) -> Message {
        let mut m = Message::text("assistant", "working");
        m.tool_calls = vec![call(call_id)];
        m
    }

    fn head() -> Event {
        let mut manifest = crate::manifest::Manifest::foreign(&cfg());
        manifest.context = super::super::OPTCHAT.into();
        Event::Head {
            id: "s".into(),
            harness: "construct".into(),
            version: "test".into(),
            cwd: "/".into(),
            model: "m".into(),
            manifest,
            parent_session: None,
            parent_call_id: None,
        }
    }

    fn memory(fit: Fit) -> Event {
        match fit {
            Fit::Changed(event) => *event,
            other => panic!("expected a change, got {other:?}"),
        }
    }

    fn leaves_of(event: &Event) -> &[MemoryNode] {
        match event {
            Event::Memory { leaves, .. } => leaves,
            other => panic!("expected a memory event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_turn_start_writes_long_leaves_and_the_view_reaches_the_turn() {
        let mut ctx = Optchat::new(vec![]);
        for m in [
            Message::system("system"),
            Message::user("start"),
            long("assistant", 600),
            Message::tool("c1", "r".repeat(600)),
            long("assistant", 600),
            Message::user("next"),
        ] {
            ctx.push(m);
        }
        let (cfg, fake, size) = (cfg(), Fake::new("line"), size);
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Threshold).await;
        let event = memory(fit);
        assert_eq!(leaves_of(&event).iter().map(|n| n.i).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(ctx.view.frontier, 4, "the view reaches turn 2's opening");
        let Event::Memory { view: Some(nodes), merges, .. } = &event else { panic!("expected a view") };
        assert_eq!(nodes, &vec![(0, 0), (0, 1), (0, 2), (0, 3)]);
        assert_eq!(merges.len(), 2, "adjacent sibling parents");
        assert_eq!(fake.calls(), 5, "3 leaves and 2 merges");
    }

    #[tokio::test]
    async fn concurrent_calls_never_exceed_the_configured_limit() {
        let mut ctx = Optchat::new(vec![]);
        ctx.push(Message::system("system"));
        ctx.push(Message::user("start"));
        for i in 0..5 {
            ctx.push(long("assistant", 600));
            ctx.push(Message::tool(&format!("c{i}"), "r".repeat(600)));
        }
        ctx.push(Message::user("next"));
        let mut cfg = cfg();
        cfg.context.optchat.concurrency = 2;
        let fake = Fake::new("line");
        let size = size;
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Threshold).await;
        let event = memory(fit);
        assert_eq!(leaves_of(&event).len(), 10, "five answers and five results");
        assert!(fake.peak.load(Ordering::SeqCst) <= 2, "peak {}", fake.peak.load(Ordering::SeqCst));
        assert!(fake.calls() >= 10);
    }

    #[tokio::test]
    async fn a_short_turn_with_nothing_to_merge_makes_no_calls() {
        let mut ctx = Optchat::new(vec![]);
        for m in [Message::system("system"), Message::text("assistant", "a"), Message::user("b")] {
            ctx.push(m);
        }
        let (cfg, fake, size) = (cfg(), Fake::new("line"), size);
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Threshold).await;
        let event = memory(fit);
        assert!(leaves_of(&event).is_empty());
        assert_eq!(ctx.view.frontier, 1);
        assert_eq!(fake.calls(), 0);
    }

    #[tokio::test]
    async fn a_failed_leaf_still_advances_and_is_marked_a_fallback() {
        let mut ctx = Optchat::new(vec![]);
        for m in [Message::system("system"), Message::user("start"), long("assistant", 600), Message::user("next")] {
            ctx.push(m);
        }
        let (cfg, mut fake, size) = (cfg(), Fake::new("line"), size);
        fake.fail_at = Some(0);
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Threshold).await;
        let event = memory(fit);
        let leaves = leaves_of(&event);
        assert_eq!(leaves.len(), 1);
        assert!(leaves[0].fallback);
        assert!(leaves[0].text.starts_with("[unsummarized] "));
        assert_eq!(ctx.view.frontier, 2);
    }

    #[tokio::test]
    async fn a_long_view_batches_down_to_at_most_half_of_high() {
        let mut ctx = Optchat::new(vec![]);
        ctx.push(Message::system("system"));
        for i in 0..20 {
            ctx.push(Message::text("assistant", format!("m{i}")));
        }
        ctx.push(Message::user("end"));
        let mut cfg = cfg();
        cfg.context.optchat.high = 200;
        cfg.context.optchat.concurrency = 2;
        let old: Vec<String> = (0..20).map(|i| entry_text(&ctx.entries[i])).collect();
        let fake = Fake::new("merged line");
        let size = size;
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Threshold).await;
        let event = memory(fit);
        let Event::Memory { view: Some(nodes), .. } = &event else { panic!("expected a view") };
        assert!(nodes.len() < 21, "a batch replaced pairs: {nodes:?}");
        assert!(nodes.windows(2).all(|w| w[0] != w[1]));
        let bytes: u64 =
            nodes.iter().filter_map(|&(l, i)| ctx.tree.line(&ctx.entries, l, i)).map(|l| l.len() as u64 + 1).sum();
        assert!(bytes <= cfg.context.optchat.high / 2, "{bytes} bytes, high {}", cfg.context.optchat.high);

        // The leaves a batch did not touch keep their exact line.
        for &(l, i) in nodes {
            if l == 0 {
                let expected = format!("{}+1|{}", i, old[i as usize]);
                assert_eq!(ctx.tree.line(&ctx.entries, l, i).as_deref(), Some(expected.as_str()));
            }
        }
    }

    #[tokio::test]
    async fn a_batched_fit_replays_identically_through_strict_replay() {
        let mut ctx = Optchat::new(vec![]);
        let mut events = vec![head()];
        for m in [
            Message::system("system"),
            Message::user("one"),
            Message::text("assistant", "answer one"),
            Message::user("two"),
            Message::text("assistant", "answer two"),
        ] {
            events.push(Event::for_message(m.clone()));
            ctx.push(m);
        }
        let mut cfg = cfg();
        cfg.context.optchat.high = 10;
        let fake = Fake::new("ok");
        let size = size;
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Threshold).await;
        let event = memory(fit);
        events.push(event);
        let replayed = crate::context::replay(events.iter(), Check::Strict).expect("a strict replay");
        assert_eq!(replayed.messages(), ctx.messages());
    }

    #[tokio::test]
    async fn a_threshold_fit_folds_older_rounds_and_keeps_the_opening_raw() {
        let mut ctx = Optchat::new(vec![]);
        ctx.push(Message::system("system"));
        ctx.push(Message::user("open"));
        for (n, id) in [(1, "c1"), (2, "c2"), (3, "c3"), (4, "c4")] {
            ctx.push(assistant_with(id));
            ctx.push(Message::tool(id, format!("result {n} {}", "z".repeat(60))));
        }
        let mut cfg = cfg();
        cfg.compaction.threshold = 0.01;
        let fake = Fake::new("line");
        let size_fn = size;
        let fit = ctx.fit(&env(&cfg, &fake, &size_fn), Reason::Threshold).await;
        let event = memory(fit);
        let Event::Memory { view: Some(_), .. } = &event else { panic!("the fold moved the view") };

        // Entries: 0 user, then (assistant, tool) × 4. The 4th round starts at 7.
        assert_eq!(ctx.view.frontier, 7);
        let out = ctx.messages();
        assert_eq!(out[1].parts.last().map(|p| p.text.as_str()), Some("open"), "the opening stays raw");
        let out_text: String = out[1].parts.iter().map(|p| p.text.as_str()).collect();
        assert!(out_text.contains("1+1|assistant: working"), "{out_text}");
        assert!(out_text.contains("4+1|result: result 2"), "{out_text}");

        // The raw tail is the newest round, with its results after its call.
        assert_eq!(out[2].role, "assistant");
        assert_eq!(out[2].tool_calls[0].id, "c4");
        assert_eq!(out[3].role, "tool");
        assert_eq!(out[3].tool_call_id.as_deref(), Some("c4"));
        assert_eq!(out.len(), 4, "system, the user message, the newest round's two messages");
        assert_eq!(fake.calls(), 0, "short entries need no summary");
    }

    #[tokio::test]
    async fn an_overflow_with_only_the_newest_round_has_nothing_to_cut() {
        let mut ctx = Optchat::new(vec![]);
        for m in [Message::system("system"), Message::user("open"), assistant_with("c1"), Message::tool("c1", "r")] {
            ctx.push(m);
        }
        let (cfg, fake, size) = (cfg(), Fake::new("line"), size);
        let fit = ctx.fit(&env(&cfg, &fake, &size), Reason::Overflow).await;
        assert!(matches!(fit, Fit::NothingToCut), "{fit:?}");
        assert_eq!(ctx.view.frontier, 0);
    }
}
