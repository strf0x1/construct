//! Session-learning orchestration owned by `Agent`: live in-process passes
//! triggered by turn/tool-call counters (or `/learn`), interactive exit/switch
//! handoff for any unreviewed tail, and non-interactive detached handoff.
//!
//! The one design choice a reader must know: interactive and non-interactive
//! exit handoffs differ on `reviewed_seq`. An interactive session anchors its
//! review watermark at open/resume and after each live pass so exiting hands
//! only the unreviewed tail to `construct learn`; a headless or ACP run leaves
//! `reviewed_seq` alone so debounced multi-turn `-c` chains review all
//! unreviewed turns together.

use super::{Agent, Event, Events};
use crate::config::{Config, LearnConfig, Paths};
use crate::learn;
use crate::llm::Message;
use crate::tools::ToolSources;

/// Per-session learning counters and live-review watermark.
pub(super) struct Progress {
    user_turns: u32,
    tool_calls: u32,
    turns_since_memory: u32,
    tool_calls_since_skill: u32,
    reviewed_messages: usize,
    reviewed_seq: i64,
}

impl Progress {
    pub(super) fn new(user_turns: u32, tool_calls: u32, reviewed_messages: usize, reviewed_seq: i64) -> Self {
        Self {
            user_turns,
            tool_calls,
            turns_since_memory: 0,
            tool_calls_since_skill: 0,
            reviewed_messages,
            reviewed_seq,
        }
    }

    pub(super) fn on_user_turn(&mut self) {
        self.user_turns += 1;
        self.turns_since_memory += 1;
    }

    pub(super) fn on_tool_call(&mut self) {
        self.tool_calls += 1;
        self.tool_calls_since_skill += 1;
    }

    pub(super) fn user_turns(&self) -> u32 {
        self.user_turns
    }

    fn live_kind(&self, cfg: &LearnConfig, force: bool) -> Option<learn::Kind> {
        let mem = self.turns_since_memory >= cfg.memory_every_turns;
        let skill = self.tool_calls_since_skill >= cfg.skill_every_tool_calls;
        match (mem || force, skill || force) {
            (true, true) => Some(learn::Kind::Both),
            (true, false) => Some(learn::Kind::Memory),
            (false, true) => Some(learn::Kind::Skills),
            (false, false) => None,
        }
    }

    /// Snapshot the unreviewed log tail for a live pass and advance the
    /// watermark if the counters (or `force`) say it is time to learn.
    fn take_live(
        &mut self,
        cfg: &LearnConfig,
        force: bool,
        log: &[Message],
        last_seq: i64,
    ) -> Option<(learn::Kind, Vec<Message>)> {
        let kind = self.live_kind(cfg, force)?;
        let slice = log.get(self.reviewed_messages..).filter(|s| !s.is_empty())?.to_vec();
        self.turns_since_memory = 0;
        self.tool_calls_since_skill = 0;
        self.reviewed_messages = log.len();
        self.reviewed_seq = last_seq;
        Some((kind, slice))
    }

    /// Return the watermark seq to record before spawning a detached learner
    /// when the unreviewed tail earned a review.
    fn finish_handoff(&self, log: &[Message]) -> Option<i64> {
        let tail = log.get(self.reviewed_messages..).filter(|t| !t.is_empty())?;
        let users = tail.iter().filter(|m| m.role == "user").count() as u32;
        let calls = tail.iter().map(|m| m.tool_calls.len() as u32).sum();
        learn::kind_for(users, calls).map(|_| self.reviewed_seq)
    }

    fn should_spawn_detached(&self) -> bool {
        learn::kind_for(self.user_turns, self.tool_calls).is_some()
    }
}

impl Agent {
    /// Run a background in-process learning pass if the counters reached a
    /// threshold (or `force` is set by `/learn`). Emits `Event::Learn` updates
    /// on `tx`, which should belong to this session so late results cannot land
    /// in a different conversation.
    pub fn maybe_learn(&mut self, tx: &Events, force: bool) {
        if !self.cfg.learn.enabled && !force {
            return;
        }
        let upto = self.journal.last_seq();
        let Some((kind, slice)) = self.learn.take_live(&self.cfg.learn, force, self.context.log(), upto) else {
            return;
        };
        let pass = LivePass {
            cfg: self.cfg.clone(),
            paths: self.paths.clone(),
            tx: tx.clone(),
            id: self.journal.session.clone(),
            upto,
            kind,
            force,
            slice,
            sources: self.sources.clone(),
        };
        tokio::spawn(async move {
            run_live(pass).await;
        });
    }

    /// Hand any unreviewed tail of an interactive session to a detached
    /// `construct learn` process when switching sessions or exiting.
    pub fn finish_learning(&self) {
        if !self.cfg.learn.enabled {
            return;
        }
        if let Some(seq) = self.learn.finish_handoff(self.context.log()) {
            save_reviewed_seq(&self.paths, &self.journal.session, seq);
            learn::spawn_detached(&self.journal.session);
        }
    }

    /// Spawn a detached `construct learn` process when a non-interactive
    /// session earned a review, leaving `reviewed_seq` untouched for debounced
    /// multi-turn continuations.
    pub fn spawn_detached_learning(&self) {
        if !self.cfg.learn.enabled {
            return;
        }
        if self.learn.should_spawn_detached() {
            learn::spawn_detached(&self.journal.session);
        }
    }
}

fn save_reviewed_seq(paths: &Paths, session: &str, seq: i64) {
    let mut st = learn::state(paths);
    st["reviewed_seq"][session] = serde_json::json!(seq);
    learn::save_state(paths, &st);
}

struct LivePass {
    cfg: Config,
    paths: Paths,
    tx: Events,
    id: String,
    upto: i64,
    kind: learn::Kind,
    force: bool,
    slice: Vec<Message>,
    sources: ToolSources,
}

async fn run_live(pass: LivePass) {
    let LivePass { cfg, paths, tx, id, upto, kind, force, slice, sources } = pass;
    let _ = tx.send(Event::Learn { status: "learning from this session".into() });
    if let Some(r) = learn::distill(&cfg, &paths, &id, &slice).await {
        let status = match r {
            Ok(o) => {
                learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
                if o.actions.is_empty() { "learned nothing new".to_string() } else { o.summary }
            }
            Err(e) => format!("learning failed: {e}"),
        };
        save_reviewed_seq(&paths, &id, upto);
        let _ = tx.send(Event::Learn { status });
        return;
    }
    let gate = learn::gate(&cfg, &slice).await;
    learn::log_line(&paths, &format!("session {id} (live): gate {} {} -> {:?}", gate.source, gate.note, gate.kind));
    // The counters said "time to look"; the gate says whether there's anything to see.
    let kind = match (gate.kind, force) {
        (Some(k), _) => k,
        (None, true) => kind,
        (None, false) => {
            save_reviewed_seq(&paths, &id, upto);
            let _ = tx.send(Event::Learn { status: "nothing new to learn".into() });
            return;
        }
    };
    let status = match learn::review(&cfg, &paths, &id, &slice, kind, &sources).await {
        Ok(o) => {
            learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
            if o.actions.is_empty() {
                "learned nothing new".to_string()
            } else {
                format!(
                    "learned: {}",
                    o.actions.iter().map(|a| a.split(':').next().unwrap_or("")).collect::<Vec<_>>().join(", ")
                )
            }
        }
        Err(e) => format!("learning failed: {e}"),
    };
    save_reviewed_seq(&paths, &id, upto);
    let _ = tx.send(Event::Learn { status });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_kind_tracks_memory_and_skill_thresholds_and_resets() {
        let cfg = LearnConfig { memory_every_turns: 2, skill_every_tool_calls: 3, ..LearnConfig::default() };
        let mut p = Progress::new(0, 0, 1, 2);
        let log = vec![Message::system("sys"), Message::user("u1"), Message::user("u2")];
        assert_eq!(p.live_kind(&cfg, false), None);
        assert_eq!(p.live_kind(&cfg, true), Some(learn::Kind::Both));

        p.on_user_turn();
        assert_eq!(p.live_kind(&cfg, false), None);
        p.on_user_turn();
        assert_eq!(p.live_kind(&cfg, false), Some(learn::Kind::Memory));

        for _ in 0..3 {
            p.on_tool_call();
        }
        assert_eq!(p.live_kind(&cfg, false), Some(learn::Kind::Both));
        assert_eq!(p.finish_handoff(&log), Some(2));

        let (kind, slice) = p.take_live(&cfg, false, &log, 12).expect("thresholds reached");
        assert_eq!(kind, learn::Kind::Both);
        assert_eq!(slice.len(), 2);
        assert_eq!(p.live_kind(&cfg, false), None);
        assert_eq!(p.finish_handoff(&log), None);
        assert_eq!(p.user_turns(), 2);
        assert_eq!(p.tool_calls, 3);

        for _ in 0..3 {
            p.on_tool_call();
        }
        assert_eq!(p.live_kind(&cfg, false), Some(learn::Kind::Skills));
    }
}
