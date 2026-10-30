//! What the running delegated children are doing, as plain state: the dock
//! (`dock.rs`) draws a row of it and the peek panel draws all of it.
//!
//! The design choice: this is a read-model of the `Event::Child` stream and
//! the job events, kept small on purpose. Each child keeps a ring of its last
//! steps and the text it is saying now; the journal has the whole run, so
//! nothing here grows with it.

use std::collections::VecDeque;
use std::time::Instant;

/// Steps kept per child.
const LOG: usize = 50;
/// Bytes of the text being streamed, and of a finished paragraph kept in the log.
const SAID: usize = 4000;
const KEPT: usize = 240;

pub(super) enum Entry {
    Tool {
        id: String,
        name: String,
        detail: String,
        /// `(ok, ms)` once it ended.
        done: Option<(bool, u64)>,
    },
    /// What the child said before it went on to a tool.
    Said(String),
}

pub(super) struct Child {
    /// The delegating call's id, which every `Event::Child` carries.
    pub(super) call: String,
    /// Set when the child runs as a background job.
    pub(super) job: Option<String>,
    pub(super) label: String,
    pub(super) started: Instant,
    pub(super) log: VecDeque<Entry>,
    /// The text of the response being streamed.
    pub(super) said: String,
}

impl Child {
    fn new(call: &str, label: String) -> Self {
        Self { call: call.into(), job: None, label, started: Instant::now(), log: VecDeque::new(), said: String::new() }
    }

    fn push(&mut self, e: Entry) {
        if self.log.len() == LOG {
            self.log.pop_front();
        }
        self.log.push_back(e);
    }

    /// A tool call supersedes the words that led up to it.
    pub(super) fn tool_start(&mut self, id: String, name: String, detail: String) {
        let said = std::mem::take(&mut self.said);
        if !said.trim().is_empty() {
            self.push(Entry::Said(crate::llm::truncate(said.trim(), KEPT)));
        }
        self.push(Entry::Tool { id, name, detail, done: None });
    }

    pub(super) fn tool_end(&mut self, id: &str, ok: bool, ms: u64) {
        let hit = self.log.iter_mut().rev().find(|e| matches!(e, Entry::Tool { id: i, .. } if i == id));
        if let Some(Entry::Tool { done, .. }) = hit {
            *done = Some((ok, ms));
        }
    }

    pub(super) fn say(&mut self, text: &str) {
        self.said.push_str(text);
        // Only the tail matters; cut at a char boundary.
        if self.said.len() > SAID {
            let cut = self.said.ceil_char_boundary(self.said.len() - SAID);
            self.said.drain(..cut);
        }
    }

    /// The last tool the child started, running or not.
    pub(super) fn last_tool(&self) -> Option<(&str, &str)> {
        self.log.iter().rev().find_map(|e| match e {
            Entry::Tool { name, detail, .. } => Some((name.as_str(), detail.as_str())),
            Entry::Said(_) => None,
        })
    }

    /// The last non-empty line of what it is saying.
    pub(super) fn said_line(&self) -> &str {
        self.said.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
    }
}

/// The open peek panel: which child, how far back from the newest line, and
/// when a cancel was asked for (it waits for a `y`).
#[derive(Default)]
pub(super) struct Peek {
    pub(super) sel: usize,
    pub(super) back: usize,
    pub(super) armed: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Dock {
    /// Background jobs running, shell commands too: the status line's count.
    pub(super) running: usize,
    /// Children running now, foreground and background, oldest first.
    pub(super) children: Vec<Child>,
    pub(super) peek: Option<Peek>,
    /// The session's jobs, to cancel one while a turn runs.
    pub(super) jobs: Option<crate::jobs::Jobs>,
}

impl Dock {
    /// Whether anything on screen changes with the clock.
    pub(super) fn active(&self) -> bool {
        !self.children.is_empty()
    }

    pub(super) fn has(&self, call: &str) -> bool {
        self.children.iter().any(|c| c.call == call)
    }

    pub(super) fn child_mut(&mut self, call: &str) -> Option<&mut Child> {
        self.children.iter_mut().find(|c| c.call == call)
    }

    pub(super) fn add(&mut self, call: &str, label: String) {
        if !self.has(call) {
            self.children.push(Child::new(call, label));
        }
    }

    /// The child's work is over.
    pub(super) fn finish(&mut self, call: &str) {
        self.children.retain(|c| c.call != call);
        match &mut self.peek {
            _ if self.children.is_empty() => self.peek = None,
            Some(p) => p.sel = p.sel.min(self.children.len() - 1),
            None => {}
        }
    }

    pub(super) fn finish_job(&mut self, job: &str) {
        if let Some(call) = self.children.iter().find(|c| c.job.as_deref() == Some(job)).map(|c| c.call.clone()) {
            self.finish(&call);
        }
    }

    /// Whether the call's child is a background job's, which outlives the call.
    pub(super) fn is_job(&self, call: &str) -> bool {
        self.children.iter().any(|c| c.call == call && c.job.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_supersedes_the_words_before_it_and_ends_with_its_result() {
        let mut c = Child::new("c1", "scout".into());
        c.say("let me look\nat the tree");
        assert_eq!(c.said_line(), "at the tree");
        c.tool_start("t1".into(), "read".into(), "a.rs".into());
        assert_eq!(c.said_line(), "");
        assert_eq!(c.last_tool(), Some(("read", "a.rs")));
        c.tool_end("t1", true, 12);
        assert!(matches!(c.log[1], Entry::Tool { done: Some((true, 12)), .. }));
        assert!(matches!(&c.log[0], Entry::Said(s) if s.starts_with("let me look")));
    }

    #[test]
    fn the_log_is_a_ring_and_the_text_a_tail() {
        let mut c = Child::new("c1", "scout".into());
        for i in 0..LOG + 10 {
            c.tool_start(format!("t{i}"), "shell".into(), "ls".into());
        }
        assert_eq!(c.log.len(), LOG);
        c.say(&"é".repeat(SAID));
        assert!(c.said.len() <= SAID);
    }

    #[test]
    fn finishing_clamps_or_closes_the_peek() {
        let mut d = Dock::default();
        d.add("a", "x".into());
        d.add("b", "y".into());
        d.peek = Some(Peek { sel: 1, ..Default::default() });
        d.finish("b");
        assert_eq!(d.peek.as_ref().map(|p| p.sel), Some(0));
        d.finish("a");
        assert!(d.peek.is_none() && !d.active());
    }

    #[test]
    fn a_job_child_is_found_by_its_job_and_outlives_its_call() {
        let mut d = Dock::default();
        d.add("c1", "scout".into());
        if let Some(c) = d.child_mut("c1") {
            c.job = Some("j1".into());
        }
        assert!(d.is_job("c1"));
        d.finish_job("j1");
        assert!(d.children.is_empty());
    }
}
