//! The `optchat` strategy (ADR 0043): every message stays in the log, a tree
//! of short lines covers it, and the model sees the view plus the current
//! turn raw. See `tree` for the lines and `view` for the request shape.
//!
//! The one choice a reader must know: `fit` is the only place the strategy
//! calls a model (`summary`), so a strategy holds no client (ADR 0042). It
//! writes the leaves a turn needs, merges parents, batches the view and folds
//! a turn's older rounds; every write lands in one `Event::Memory`.

mod fit;
mod summary;
mod tree;
mod view;

use super::{Check, Context, Env, Fit, OPTCHAT};
use crate::compact::Reason;
use crate::event::{Event, MemoryNode};
use crate::llm::Message;
use anyhow::Context as _;
use tree::Tree;
use view::View;

pub struct Optchat {
    /// The session's system prompt, kept out of the log. The first system
    /// message pushed is it; resume swaps in the current one with `set_system`.
    system: Option<Message>,
    /// Every other message, in order: the log.
    entries: Vec<Message>,
    tree: Tree,
    view: View,
    /// `messages()` hands out a slice, so the rendered request is kept here.
    rendered: Vec<Message>,
}

impl Optchat {
    pub fn new(messages: Vec<Message>) -> Self {
        let mut ctx =
            Self { system: None, entries: vec![], tree: Tree::default(), view: View::default(), rendered: vec![] };
        for m in messages {
            ctx.push(m);
        }
        ctx
    }

    fn refresh(&mut self) {
        self.rendered = view::render(self.system.as_ref(), &self.entries, &self.tree, &self.view);
    }

    /// One past the log's entries, for a strict range check.
    fn log_end(&self) -> u128 {
        self.entries.len() as u128
    }

    fn range(&self, l: u32, i: u64) -> anyhow::Result<u128> {
        Tree::end(l, i).with_context(|| format!("memory node ({l},{i}) has an impossible range"))
    }

    fn check_view(&self, nodes: &[(u32, u64)]) -> anyhow::Result<()> {
        let mut pos = 0u128;
        for &(l, i) in nodes {
            let width = 1u128.checked_shl(l).with_context(|| format!("memory view node ({l},{i}) is malformed"))?;
            let start = u128::from(i).checked_mul(width).with_context(|| format!("memory node ({l},{i}) overflows"))?;
            anyhow::ensure!(
                start == pos,
                "memory view does not cover entry {pos} next: node ({l},{i}) starts at {start}"
            );
            let end = start + width;
            anyhow::ensure!(
                end <= self.log_end(),
                "memory view node ({l},{i}) covers entries past the log's {} entries",
                self.entries.len()
            );
            anyhow::ensure!(self.tree.built(&self.entries, l, i), "memory view node ({l},{i}) is not built");
            pos = end;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Context for Optchat {
    fn name(&self) -> &'static str {
        OPTCHAT
    }

    fn messages(&self) -> &[Message] {
        &self.rendered
    }

    fn push(&mut self, m: Message) {
        if self.system.is_none() && m.role == "system" {
            self.system = Some(m);
        } else {
            self.entries.push(m);
        }
        self.refresh();
    }

    fn set_system(&mut self, message: Message) -> bool {
        let recorded = self.system.is_some();
        self.system = Some(message);
        self.refresh();
        recorded
    }

    fn replay(&mut self, e: &Event, check: Check) -> anyhow::Result<()> {
        let Event::Memory { leaves, merges, view, .. } = e else {
            return Ok(());
        };
        for MemoryNode { l, i, text, fallback } in leaves {
            if check == Check::Strict {
                let end = self.range(*l, *i)?;
                anyhow::ensure!(
                    end <= self.log_end(),
                    "memory leaf ({l},{i}) covers entries past the log's {} entries",
                    self.entries.len()
                );
            }
            self.tree.insert(*l, *i, text.clone(), *fallback);
        }
        for MemoryNode { l, i, text, fallback } in merges {
            if check == Check::Strict {
                let parent = l.checked_sub(1).with_context(|| format!("memory merge ({l},{i}) has no children"))?;
                let first = i.checked_mul(2).with_context(|| format!("memory merge ({l},{i}) overflows"))?;
                let second = first.checked_add(1).with_context(|| format!("memory merge ({l},{i}) overflows"))?;
                let end = self.range(*l, *i)?;
                anyhow::ensure!(
                    end <= self.log_end(),
                    "memory merge ({l},{i}) covers entries past the log's {} entries",
                    self.entries.len()
                );
                anyhow::ensure!(
                    self.tree.built(&self.entries, parent, first) && self.tree.built(&self.entries, parent, second),
                    "memory merge ({l},{i}) has a child that is neither stored nor derived"
                );
            }
            self.tree.insert(*l, *i, text.clone(), *fallback);
        }
        if let Some(nodes) = view {
            if check == Check::Strict {
                self.check_view(nodes)?;
            }
            self.view.set(nodes.clone());
        }
        self.refresh();
        Ok(())
    }

    async fn fit(&mut self, env: &Env<'_>, reason: Reason) -> Fit {
        self.fitting(env, reason).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Complete;
    use crate::llm::{Completion, Usage};

    /// Answers every summary call with one short line.
    struct Lines;

    #[async_trait::async_trait]
    impl Complete for Lines {
        async fn complete(&self, _: &str, _: Vec<Message>, _: Option<u32>) -> (Result<Completion, String>, Usage) {
            (
                Ok(Completion {
                    message: Message::text("assistant", "merged line"),
                    usage: Usage::default(),
                    finish_reason: "stop".into(),
                }),
                Usage::default(),
            )
        }
    }

    fn session() -> Optchat {
        let mut ctx = Optchat::new(vec![]);
        for m in [
            Message::system("system"),
            Message::user("one"),
            Message::text("assistant", "answer one"),
            Message::user("two"),
            Message::text("assistant", "answer two"),
            Message::user("three"),
        ] {
            ctx.push(m);
        }
        ctx
    }

    fn env<'a>(
        cfg: &'a crate::config::Config,
        llm: &'a dyn Complete,
        size: &'a (dyn Fn(&[Message]) -> u64 + Send + Sync),
    ) -> Env<'a> {
        Env { size, budget: 100_000, bytes_per_token: 4.0, cfg, llm }
    }

    fn memory(leaves: Vec<MemoryNode>, merges: Vec<MemoryNode>, view: Option<Vec<(u32, u64)>>) -> Event {
        Event::Memory { leaves, merges, view, summary_prompt: 0, summary_completion: 0, ms: 0 }
    }

    fn leaf(i: u64, text: &str) -> MemoryNode {
        MemoryNode { l: 0, i, text: text.into(), fallback: false }
    }

    fn head() -> Event {
        let mut manifest = crate::manifest::Manifest::foreign(&crate::config::Config::default());
        manifest.context = OPTCHAT.into();
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

    #[tokio::test]
    async fn fits_previous_turns_into_the_view_word_for_word_and_leaves_the_turn_raw() {
        let mut ctx = session();
        let (cfg, llm, size) = (crate::config::Config::default(), Lines, |_: &[Message]| 0);
        let fit = ctx.fit(&env(&cfg, &llm, &size), Reason::Threshold).await;
        let Fit::Changed(event) = fit else { panic!("expected a view change, got {fit:?}") };
        let Event::Memory { view: Some(nodes), .. } = *event else { panic!("expected a memory event") };
        assert_eq!(nodes, vec![(0, 0), (0, 1), (0, 2), (0, 3)]);
        assert_eq!(ctx.view.frontier, 4);

        let out = ctx.messages();
        assert_eq!(out.len(), 2, "system and the one user message");
        let text: String = out[1].parts.iter().map(|p| p.text.as_str()).collect();
        for line in ["0+1|user: one\n", "1+1|assistant: answer one\n", "2+1|user: two\n", "3+1|assistant: answer two\n"]
        {
            assert!(text.contains(line), "{text:?} lacks {line:?}");
        }
        assert!(text.ends_with("three"));
    }

    #[tokio::test]
    async fn a_long_entry_gets_a_written_leaf_and_no_longer_blocks_the_turn() {
        let mut ctx = Optchat::new(vec![]);
        for m in [
            Message::system("system"),
            Message::user("one"),
            Message::text("assistant", "x".repeat(600)),
            Message::user("two"),
        ] {
            ctx.push(m);
        }
        let (cfg, llm, size) = (crate::config::Config::default(), Lines, |_: &[Message]| 0);
        let fit = ctx.fit(&env(&cfg, &llm, &size), Reason::Threshold).await;
        let Fit::Changed(event) = fit else { panic!("expected a change, got {fit:?}") };
        let Event::Memory { leaves, .. } = *event else { panic!("expected a memory event") };
        assert_eq!(leaves.len(), 1);
        assert_eq!((leaves[0].l, leaves[0].i), (0, 1));
        assert_eq!(ctx.view.frontier, 2);
        let text: String = ctx.messages()[1].parts.iter().map(|p| p.text.as_str()).collect();
        assert!(text.contains("1+1|merged line"), "{text:?}");
        assert!(!text.contains("xxxx"), "the long entry is no longer raw");
        assert_eq!(ctx.messages()[1].parts.last().map(|p| p.text.as_str()), Some("two"));
    }

    #[test]
    fn replay_rebuilds_the_same_request_as_push_and_replay_by_hand() {
        let events = vec![
            head(),
            Event::for_message(Message::system("system")),
            Event::for_message(Message::user("one")),
            Event::for_message(Message::text("assistant", "answer one")),
            Event::for_message(Message::user("two")),
            memory(vec![], vec![], Some(vec![(0, 0), (0, 1)])),
            Event::for_message(Message::text("assistant", "answer two")),
            Event::for_message(Message::user("three")),
        ];
        let replayed = crate::context::replay(events.iter(), Check::Lenient).expect("a lenient replay");

        let mut by_hand = Optchat::new(vec![]);
        for e in &events {
            match e.message() {
                Some(m) => by_hand.push(m.clone()),
                None => by_hand.replay(e, Check::Lenient).expect("the memory event applies"),
            }
        }
        assert_eq!(replayed.messages(), by_hand.messages());
    }

    #[test]
    fn strict_replay_refuses_a_node_past_the_log() {
        let events = [
            head(),
            Event::for_message(Message::system("system")),
            Event::for_message(Message::user("one")),
            memory(vec![leaf(5, "late")], vec![], None),
        ];
        let err = crate::context::replay(events.iter(), Check::Strict).err().expect("a leaf past the log refuses");
        assert!(err.to_string().contains("past the log's 1 entries"), "{err}");
        crate::context::replay(events.iter(), Check::Lenient).expect("lenient applies it");
    }

    #[test]
    fn strict_replay_refuses_a_merge_whose_children_are_not_built() {
        let events = [
            head(),
            Event::for_message(Message::system("system")),
            Event::for_message(Message::user("one")),
            Event::for_message(Message::text("assistant", "x".repeat(600))),
            memory(vec![], vec![MemoryNode { l: 1, i: 0, text: "merge".into(), fallback: false }], None),
        ];
        let err = crate::context::replay(events.iter(), Check::Strict).err().expect("an unbuilt merge refuses");
        assert!(err.to_string().contains("neither stored nor derived"), "{err}");
        crate::context::replay(events.iter(), Check::Lenient).expect("lenient applies it");
    }

    #[test]
    fn strict_replay_refuses_a_view_with_a_hole_or_an_unbuilt_node() {
        let entries = [
            head(),
            Event::for_message(Message::system("system")),
            Event::for_message(Message::user("one")),
            Event::for_message(Message::text("assistant", "answer one")),
            Event::for_message(Message::user("two")),
        ];
        let hole = [
            entries[0].clone(),
            entries[1].clone(),
            entries[2].clone(),
            entries[3].clone(),
            entries[4].clone(),
            memory(vec![], vec![], Some(vec![(0, 0), (0, 2)])),
        ];
        let err = crate::context::replay(hole.iter(), Check::Strict).err().expect("a hole refuses");
        assert!(err.to_string().contains("does not cover entry 1 next"), "{err}");
        crate::context::replay(hole.iter(), Check::Lenient).expect("lenient applies it");

        // Entry 1 is too long to derive and has no written leaf.
        let long = [
            head(),
            Event::for_message(Message::system("system")),
            Event::for_message(Message::user("x".repeat(600))),
            Event::for_message(Message::text("assistant", "answer")),
            Event::for_message(Message::user("two")),
            memory(vec![], vec![], Some(vec![(0, 0)])),
        ];
        let err = crate::context::replay(long.iter(), Check::Strict).err().expect("an unbuilt view node refuses");
        assert!(err.to_string().contains("is not built"), "{err}");
    }

    #[test]
    fn a_head_naming_optchat_builds_the_strategy() {
        let ctx = crate::context::replay([head()].iter(), Check::Lenient).expect("a head-only replay");
        assert_eq!(ctx.name(), OPTCHAT);
    }
}
