//! Historical UI projection, separate from the compacted model messages.
//! A ready notification atomically replaces all session-specific UI state.

use super::app::{App, Block, Kind};
use super::theme;
use super::worker::Ready;
use crate::event::Event;

impl App {
    pub(super) fn restore(&mut self, mut ready: Ready) {
        let ack = ready.ack.take();
        self.blocks.append(&mut self.held);
        // The first frame is interactive before initialization finishes.
        // Preserve everything the user has already typed or opened there.
        let initial = if !self.ready {
            Some((
                std::mem::take(&mut self.blocks),
                std::mem::take(&mut self.input),
                self.cursor,
                std::mem::take(&mut self.history),
                self.hist_pos,
                self.picker.take(),
            ))
        } else {
            None
        };
        self.blocks.clear();
        self.cache.clear();
        self.input.clear();
        self.cursor = 0;
        self.history.clear();
        self.hist_pos = None;
        self.scroll = 0;
        self.last_total = 0;
        self.turn.restored(!self.ready);
        self.learning = None;
        self.toast = None;
        self.prompts.clear();
        self.dock = Default::default();
        self.dock.jobs = Some(ready.jobs);
        self.generation = ready.tag;
        self.session = ready.session;
        self.cfg = ready.cfg;
        self.catalog = Some(ready.catalog);
        self.canceller = Some(ready.canceller);
        self.groups = ready.groups;
        self.ready = true;
        self.palette = None;
        self.picker = None;
        let started: std::collections::BTreeSet<_> = ready
            .entries
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolStart { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let mut raw = false;
        for e in ready.entries {
            match e.event {
                Event::Submitted { text } => {
                    self.history.push(text.clone());
                    self.push(Block::new(Kind::User, text));
                    raw = true;
                }
                Event::User { message } => {
                    if !raw {
                        let text = message
                            .content
                            .unwrap_or_default()
                            .split("\n\n<system-reminder>")
                            .next()
                            .unwrap_or("")
                            .to_string();
                        self.history.push(text.clone());
                        self.push(Block::new(Kind::User, text));
                    }
                    raw = false;
                }
                Event::Assistant { message } => {
                    if let Some(text) = message.reasoning.filter(|s| !s.is_empty()) {
                        self.push(Block::new(Kind::Reasoning, text));
                    }
                    if let Some(text) = message.content.filter(|s| !s.is_empty()) {
                        self.push(Block::new(Kind::Assistant, text));
                    }
                    for call in &message.tool_calls {
                        if !started.contains(&call.id) {
                            self.on_event(Event::ToolStart {
                                id: call.id.clone(),
                                name: call.function.name.clone(),
                                args: call.function.arguments.clone(),
                            });
                        }
                    }
                }
                Event::ToolResult { id, message } => {
                    if let Some(b) = self
                        .blocks
                        .iter_mut()
                        .find(|b| b.kind == Kind::Tool && b.id == id && b.status == super::app::Status::Running)
                    {
                        b.text = message.content.unwrap_or_default();
                        b.status =
                            if b.text.starts_with("Error:") { super::app::Status::Err } else { super::app::Status::Ok };
                    }
                }
                Event::Recovered { message } => self.notice(theme::YELLOW, message),
                ev @ (Event::ToolStart { .. }
                | Event::ToolEnd { .. }
                | Event::Compacted { .. }
                | Event::JobStart(_)
                | Event::JobEnd(_)
                | Event::Child { .. }) => self.on_event(ev),
                _ => {}
            }
        }
        for b in &mut self.blocks {
            if b.status == super::app::Status::Running {
                b.status = super::app::Status::Err;
            }
        }
        if let Some((blocks, input, cursor, history, hist_pos, picker)) = initial {
            self.blocks.extend(blocks);
            self.input = input;
            self.cursor = cursor;
            self.history.extend(history);
            self.hist_pos = hist_pos;
            self.picker = picker;
            if let Some(p) = self.picker.as_mut() {
                p.refresh(&self.cfg);
            }
        }
        self.totals = ready.spent;
        self.context_pct = self.context_pct_now();
        self.refresh_palette();
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }

    pub(super) fn switch_blocked(&mut self) -> bool {
        let blocked = self.turn.blocks_switch()
            || !self.prompts.is_empty()
            || self.dock.jobs.as_ref().is_some_and(|j| j.running() > 0);
        if blocked {
            self.notice(theme::YELLOW,
                "Cannot switch sessions while a turn, queued prompt, permission request or background job is active. Wait or cancel it first.");
        }
        blocked
    }
}

/// Reattach delegated transcript events under their recorded call. They were
/// stored once under the child ID, rather than duplicated in the parent's log.
pub(super) fn with_children(
    entries: Vec<crate::journal::Entry>,
    db: &crate::journal::Journal,
) -> anyhow::Result<Vec<crate::journal::Entry>> {
    let mut out = vec![];
    for e in entries {
        if let Event::ToolEnd { id, data: Some(data), .. } = &e.event
            && let Some(child) = data.get("session").and_then(|v| v.as_str())
        {
            for row in db.strict(child)? {
                out.push(crate::journal::Entry {
                    event: Event::Child { id: id.clone(), inner: Box::new(row.event) },
                    ..row
                });
            }
        }
        out.push(e);
    }
    Ok(out)
}
