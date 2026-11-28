//! Fold live events into transcript blocks, turn state, totals and status.
//! Historical replay uses durable messages separately because streaming deltas
//! are deliberately absent.

use super::App;
use crate::theme;
use crate::transcript::Item;
use crate::turn::Phase;
use construct_core::agent::Event;
use std::time::Instant;

impl App {
    pub(super) fn on_event(&mut self, ev: Event) {
        self.last_activity = Instant::now();
        // Anything billed counts, a summary's included.
        self.totals.add(&ev);
        let any_tool = self.transcript.on_event(&ev, self.catalog.as_deref(), &mut self.motion);
        match ev {
            // The record, not the screen: the UI already shows what these carry.
            Event::Manifest(_)
            | Event::Head { .. }
            | Event::Permission { .. }
            // Shown when the prompt is answered.
            | Event::Access { .. }
            | Event::Operation { .. }
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
            | Event::Request { .. }
            | Event::ToolOutput { .. }
            | Event::Compacted { .. }
            // The turn it starts or joins says it.
            | Event::JobsDelivered(_) => {}
            Event::Submitted(_) => self.turn.submitted(),
            Event::TurnStart => {
                // The agent clears its cancel flag as a turn starts.
                self.quit_on_end = false;
                self.turn.start(Instant::now());
            }
            Event::Route { groups, skills, source, ms, note } => {
                for g in &groups {
                    if !self.groups.contains(g) {
                        self.groups.push(g.clone());
                    }
                }
                self.turn.set_phase(Phase::Waiting);
                if !skills.is_empty() || !groups.is_empty() {
                    let mut parts = vec![];
                    if !groups.is_empty() {
                        parts.push(format!("tools +{}", groups.join(" +")));
                    }
                    if !skills.is_empty() {
                        parts.push(format!("skills {}", skills.join(", ")));
                    }
                    self.transcript
                        .push(Item::Notice(theme::FAINT, format!("↳ {} · {source} {ms}ms", parts.join(" · "))));
                }
                if !note.is_empty() && note.contains("failed") {
                    self.transcript.push(Item::Notice(theme::YELLOW, note));
                }
            }
            Event::Opened => self.turn.opened(),
            Event::Reasoning { .. } => self.turn.set_phase(Phase::Thinking),
            Event::Text { text } => self.turn.wrote(text.len()),
            Event::ToolPending { name } => self.turn.set_phase(Phase::Tool(format!("preparing {name}"))),
            Event::ToolStart { name, .. } => self.turn.set_phase(Phase::Tool(name)),
            Event::ToolEnd { .. } => {
                if !any_tool {
                    self.turn.set_phase(Phase::Waiting);
                }
            }
            Event::Usage { .. } => {
                let (old, now) = (self.context_pct, self.context_pct_now());
                self.context_pct = now;
                self.motion.gauge(old, now);
            }
            Event::Model { model, context_window, .. } => {
                let _ = self.cfg.select_model(&model); // the agent switched, so it resolves
                self.context_pct = self.context_pct_now();
                let label = self.cfg.entry().map(|e| e.label.as_str()).filter(|l| !l.is_empty());
                let what = label.map_or(model.clone(), |l| format!("{model} ({l})"));
                self.transcript.push(Item::Notice(
                    theme::ACCENT,
                    format!("model → {what} · {} ctx", construct_core::models::short_tokens(context_window as u64)),
                ));
            }
            Event::TurnEnd { steps, ms, interrupted, .. } => {
                self.turn.end();
                // The turn may have written a skill: list it if `/` is up.
                if self.composer.invalidate_palette() {
                    self.refresh_palette();
                }
                let secs = ms as f32 / 1000.0;
                if std::mem::take(&mut self.quit_on_end) && !interrupted {
                    // The ctrl-c came after the agent's last look: the turn
                    // finished anyway, so the key arms quit instead.
                    self.arm_quit();
                }
                if interrupted {
                    self.transcript.push(Item::Notice(theme::YELLOW, format!("■ interrupted after {secs:.1}s")));
                } else if steps > 1 {
                    self.transcript.push(Item::Notice(theme::FAINT, format!("· {steps} steps · {secs:.1}s")));
                }
            }
            Event::Error { message } => {
                self.turn.error();
                self.transcript.push(Item::Notice(theme::RED, format!("✗ {message}")));
            }
            Event::JobStart(_) => self.jobs_running += 1,
            Event::JobEnd(_) => self.jobs_running = self.jobs_running.saturating_sub(1),
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
