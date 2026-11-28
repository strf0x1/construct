//! Folding live agent events and replaying durable journal entries into
//! [`Transcript`] blocks.

use super::{Block, Item, Kind, Transcript, tool};
use crate::motion::Motion;
use crate::theme;
use construct_core::agent::Event;
use construct_core::jobs::Status as JobStatus;
use construct_core::journal::Entry as JournalEntry;
use construct_core::tools::{Catalog, OutputKind};
use std::collections::BTreeSet;

impl Transcript {
    /// Fold one live agent event into the transcript and notify `motion`.
    /// Returns true if any tool block is still running after this event.
    pub(crate) fn on_event(&mut self, ev: &Event, catalog: Option<&Catalog>, motion: &mut Motion) -> bool {
        match ev {
            Event::Reasoning { text } => match self.blocks.last_mut().filter(|b| b.is_running_reasoning()) {
                Some(b) => b.append_text(text),
                None => {
                    self.push_block(Block::running(Kind::Reasoning, text.as_str()));
                }
            },
            Event::Text { text } => {
                self.close_reasoning();
                let idx = match self.blocks.last_mut().filter(|b| b.kind == Kind::Assistant) {
                    Some(b) => {
                        b.append_text(text);
                        self.blocks.len() - 1
                    }
                    None => self.push_block(Block::running(Kind::Assistant, text.as_str())),
                };
                motion.wrote(idx, text);
            }
            Event::ToolPending { .. } | Event::Error { .. } => self.close_reasoning(),
            Event::ToolStart { id, name, args } => {
                self.close_reasoning();
                let d = catalog.map(|c| c.display(name)).unwrap_or_default();
                let detail = tool::primary_arg(d.primary_arg, args);
                self.push_block(Block::tool(id.clone(), name.clone(), detail, d.preview_lines));
            }
            Event::ToolOutput { id, text } => {
                if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.matches_tool(id) && b.is_running_tool()) {
                    b.push_live(text);
                    motion.live_tick();
                }
            }
            Event::ToolEnd { id, ok, output, ms, kind, .. } => {
                if let Some(i) = self.blocks.iter().rposition(|b| b.matches_tool(id)) {
                    let from_live = self.blocks[i].finish_tool(*ok, output.clone(), *ms, *kind);
                    motion.tool_done(i, *ok, *kind == OutputKind::Diff, from_live);
                }
            }
            Event::Compacted { before, after, mode, tokens_before, tokens_after, .. } => {
                let what = if *tokens_before > 0 {
                    format!("{mode}, ~{}k → ~{}k tokens", tokens_before / 1000, tokens_after / 1000)
                } else {
                    format!("{before} → {after} messages")
                };
                let idx =
                    self.push_block(Block::new(Kind::Notice(theme::ACCENT2), format!("◇ compacted history ({what})")));
                motion.compacted(idx);
            }
            Event::TurnEnd { .. } => {
                self.close_reasoning();
                for b in &mut self.blocks {
                    b.settle_running();
                }
                self.blocks.append(&mut self.held);
            }
            Event::JobStart(s) => {
                let label = construct_core::text::truncate(&s.label.replace('\n', " ⏎ "), 80);
                self.push(Item::Aside(theme::FAINT, format!("◆ job {} started · {label}", s.id)));
            }
            Event::JobEnd(j) => {
                let color = match j.status {
                    JobStatus::Completed => theme::GREEN,
                    JobStatus::Failed | JobStatus::Timeout => theme::RED,
                    JobStatus::Cancelled | JobStatus::Running | JobStatus::Interrupted | JobStatus::Waiting => {
                        theme::YELLOW
                    }
                };
                self.push(Item::Aside(
                    color,
                    format!("◆ job {} {} · {:.1}s", j.id, j.status.label(), j.ms as f64 / 1000.0),
                ));
            }
            _ => {}
        }
        self.blocks.iter().any(Block::is_running_tool)
    }

    /// Replace the transcript with replayed journal entries, preserving blocks
    /// created before initialization finished when `keep_initial` is true.
    /// Returns `(prompt_history, jobs_running)`.
    pub(crate) fn replay(
        &mut self,
        entries: &[JournalEntry],
        catalog: Option<&Catalog>,
        keep_initial: bool,
    ) -> (Vec<String>, usize) {
        self.blocks.append(&mut self.held);
        let initial = if keep_initial { std::mem::take(&mut self.blocks) } else { Vec::new() };
        self.blocks.clear();
        self.cache.clear();
        self.scroll = 0;
        self.last_total = 0;
        let started: BTreeSet<&str> = entries
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolStart { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let mut raw = false;
        let mut history = Vec::new();
        let mut jobs_running = 0usize;
        let mut no_motion = Motion::default();
        for e in entries {
            match &e.event {
                Event::Submitted(construct_core::event::Submission { text, .. }) => {
                    history.push(text.clone());
                    self.push_block(Block::new(Kind::User, text.as_str()));
                    raw = true;
                }
                Event::User { message } => {
                    if !raw {
                        let text = message
                            .content
                            .as_deref()
                            .unwrap_or("")
                            .split("\n\n<system-reminder>")
                            .next()
                            .unwrap_or("")
                            .to_string();
                        history.push(text.clone());
                        self.push_block(Block::new(Kind::User, text));
                    }
                    raw = false;
                }
                Event::Assistant { message } => {
                    if let Some(text) = message.reasoning.as_deref().filter(|s| !s.is_empty()) {
                        self.push_block(Block::new(Kind::Reasoning, text));
                    }
                    if let Some(text) = message.content.as_deref().filter(|s| !s.is_empty()) {
                        self.push_block(Block::new(Kind::Assistant, text));
                    }
                    for call in &message.tool_calls {
                        if !started.contains(call.id.as_str()) {
                            let ev = Event::ToolStart {
                                id: call.id.clone(),
                                name: call.function.name.clone(),
                                args: call.function.arguments.clone(),
                            };
                            self.on_event(&ev, catalog, &mut no_motion);
                        }
                    }
                }
                Event::ToolResult { id, message } => {
                    if let Some(b) = self.blocks.iter_mut().find(|b| b.matches_tool(id) && b.is_running_tool()) {
                        b.finish_history_tool(message.content.clone().unwrap_or_default());
                    }
                }
                Event::Recovered { message } => {
                    self.push_block(Block::new(Kind::Notice(theme::YELLOW), message.as_str()));
                }
                Event::JobStart(_) => {
                    jobs_running += 1;
                    self.on_event(&e.event, catalog, &mut no_motion);
                }
                Event::JobEnd(_) => {
                    jobs_running = jobs_running.saturating_sub(1);
                    self.on_event(&e.event, catalog, &mut no_motion);
                }
                Event::ToolStart { .. } | Event::ToolEnd { .. } | Event::Compacted { .. } => {
                    self.on_event(&e.event, catalog, &mut no_motion);
                }
                _ => {}
            }
        }
        for b in &mut self.blocks {
            b.fail_if_running();
        }
        self.blocks.extend(initial);
        (history, jobs_running)
    }
}
