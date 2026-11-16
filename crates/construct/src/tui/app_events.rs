//! Fold live events into transcript blocks and status. Historical replay uses
//! durable messages separately because streaming deltas are deliberately absent.

use super::app::*;
use super::args::primary_arg;
use super::theme;
use crate::agent::Event;
use std::time::Instant;

impl App {
    pub(super) fn on_event(&mut self, ev: Event) {
        self.last_activity = Instant::now();
        // Anything billed counts, a summary's or a child's included.
        self.totals.add(&ev);
        match ev {
            // The record, not the screen: the UI already shows what these carry.
            Event::Manifest(_)
            | Event::Head { .. }
            | Event::Permission { .. }
            // Shown when the prompt is answered, so a child's are too.
            | Event::Access { .. }
            | Event::Operation { .. }
            | Event::Ask { .. }
            | Event::System { .. }
            | Event::User { .. }
            | Event::SessionName { .. }
            | Event::Recovered { .. }
            | Event::Memory { .. }
            // The user's own `/skill` line already says it.
            | Event::Invoke { .. }
            | Event::Assistant { .. }
            | Event::ToolResult { .. }
            | Event::Note { .. }
            | Event::DeckBoundary { .. }
            | Event::Nudge { .. }
            | Event::Request { .. } => {}
            Event::Submitted { .. } => self.turn.submitted(),
            Event::TurnStart => self.turn.start(Instant::now()),
            Event::Route { groups, skills, source, ms, note } => {
                for g in &groups { if !self.groups.contains(g) { self.groups.push(g.clone()); } }
                self.turn.set_phase(Phase::Waiting);
                if !skills.is_empty() || !groups.is_empty() {
                    let mut parts = vec![];
                    if !groups.is_empty() {
                        parts.push(format!("tools +{}", groups.join(" +")));
                    }
                    if !skills.is_empty() {
                        parts.push(format!("skills {}", skills.join(", ")));
                    }
                    self.notice(theme::FAINT, format!("↳ {} · {source} {ms}ms", parts.join(" · ")));
                }
                if !note.is_empty() && note.contains("failed") {
                    self.notice(theme::YELLOW, note);
                }
            }
            Event::Opened => self.turn.opened(),
            Event::Reasoning { text } => {
                self.turn.set_phase(Phase::Thinking);
                match self.last_of(Kind::Reasoning) {
                    Some(b) if b.status == Status::Running => {
                        b.text.push_str(&text);
                        b.version += 1;
                    }
                    _ => {
                        let mut b = Block::new(Kind::Reasoning, text);
                        b.status = Status::Running;
                        self.push(b);
                    }
                }
            }
            Event::Text { text } => {
                self.turn.wrote(text.len());
                self.close_reasoning();
                match self.last_of(Kind::Assistant) {
                    Some(b) => {
                        b.text.push_str(&text);
                        b.version += 1;
                    }
                    None => {
                        let mut b = Block::new(Kind::Assistant, text);
                        b.status = Status::Running;
                        self.push(b);
                    }
                }
            }
            Event::ToolPending { name } => {
                self.close_reasoning();
                self.turn.set_phase(Phase::Tool(format!("preparing {name}")));
            }
            Event::ToolStart { id, name, args } => {
                self.close_reasoning();
                let mut b = Block::new(Kind::Tool, "");
                let d = self.catalog.as_ref().map(|c| c.display(&name)).unwrap_or_default();
                b.detail = primary_arg(d.primary_arg, &args);
                b.preview = d.preview_lines;
                b.title = name.clone();
                b.id = id;
                b.status = Status::Running;
                self.turn.set_phase(Phase::Tool(name));
                self.push(b);
            }
            Event::ToolEnd { id, ok, output, ms, kind, .. } => {
                self.end_call(&id);
                if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.kind == Kind::Tool && b.id == id) {
                    b.status = if ok { Status::Ok } else { Status::Err };
                    b.text = output;
                    b.ms = ms;
                    b.out = kind;
                    b.version += 1;
                }
                if !self.blocks.iter().any(|b| b.kind == Kind::Tool && b.status == Status::Running) {
                    self.turn.set_phase(Phase::Waiting);
                }
            }
            Event::Usage { .. } => self.context_pct = self.context_pct_now(),
            Event::Model { model, context_window, .. } => {
                let _ = self.cfg.select_model(&model); // the agent switched, so it resolves
                self.context_pct = self.context_pct_now();
                let label = self.cfg.entry().map(|e| e.label.as_str()).filter(|l| !l.is_empty());
                let what = label.map_or(model.clone(), |l| format!("{model} ({l})"));
                self.notice(theme::ACCENT, format!("model → {what} · {} ctx", crate::models::short_tokens(context_window as u64)));
            }
            Event::Compacted { before, after, mode, tokens_before, tokens_after, .. } => {
                let what = if tokens_before > 0 {
                    format!("{mode}, ~{}k → ~{}k tokens", tokens_before / 1000, tokens_after / 1000)
                } else {
                    format!("{before} → {after} messages")
                };
                self.notice(theme::ACCENT2, format!("◇ compacted history ({what})"));
            }
            Event::TurnEnd { steps, ms, interrupted, .. } => {
                self.close_reasoning();
                for b in &mut self.blocks {
                    if b.status == Status::Running {
                        b.status = if b.kind == Kind::Tool { Status::Err } else { Status::Ok };
                        b.version += 1;
                    }
                }
                self.blocks.append(&mut self.held);
                self.turn.end();
                // The turn may have written a skill: list it if `/` is up.
                if self.palette.take().is_some() {
                    self.refresh_palette();
                }
                let secs = ms as f32 / 1000.0;
                if interrupted {
                    self.notice(theme::YELLOW, format!("■ interrupted after {secs:.1}s"));
                } else if steps > 1 {
                    self.notice(theme::FAINT, format!("· {steps} steps · {secs:.1}s"));
                }
            }
            Event::Error { message } => {
                self.close_reasoning();
                self.turn.error();
                self.notice(theme::RED, format!("✗ {message}"));
            }
            Event::Child { id, inner } => self.on_child(&id, *inner),
            Event::JobStart(_) | Event::JobEnd(_) => self.on_job(&ev),
            // The turn it starts or joins says it.
            Event::JobsDelivered(_) => {}
            Event::Learn { status } => {
                if status.starts_with("learning") {
                    self.learning = Some((status, Instant::now()));
                } else {
                    self.learning = None;
                    self.toast = Some((status, theme::ACCENT2, Instant::now()));
                }
            }
        }
    }
}
