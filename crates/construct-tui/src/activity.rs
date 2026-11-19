//! The activity line above the input: what the running turn is doing, how
//! long it has run, and a plain warning when the model has gone quiet. It
//! reads only `Turn`, so a turn that is sent but not started says so.

use super::app::{App, Phase};
use super::theme;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

impl App {
    pub(super) fn activity(&self, phase: f32) -> Line<'static> {
        if !self.turn.busy() {
            return Line::default();
        }
        let since = self.last_activity.elapsed().as_secs_f32();
        let running = self.turn.running();
        let phase_now = running.map(|r| &r.phase);
        let (mut label, mut base, mut hi) = match phase_now {
            Some(Phase::Routing) => ("routing".to_string(), theme::DIM, theme::CYAN),
            Some(Phase::Waiting) => ("waiting for model".to_string(), theme::DIM, theme::ACCENT),
            Some(Phase::Thinking) => ("thinking".to_string(), theme::DIM, theme::ACCENT2),
            Some(Phase::Writing) => ("writing".to_string(), theme::DIM, theme::ACCENT),
            Some(Phase::Tool(n)) => (format!("running {n}"), theme::DIM, theme::YELLOW),
            // Sent, and the worker has not started it yet.
            None => ("starting".to_string(), theme::DIM, theme::DIM),
        };
        // Silence from the model reads as "broken"; say so plainly instead.
        if since > 6.0 && !matches!(phase_now, Some(Phase::Tool(_))) {
            label = format!("{label} · no output for {since:.0}s");
            base = theme::YELLOW;
            hi = Color::Rgb(255, 230, 180);
        }
        let mut spans = vec![Span::styled(format!("  {} ", theme::spinner(phase)), Style::new().fg(hi))];
        spans.extend(theme::shimmer(&label, phase, base, hi));
        if let Some(r) = running {
            let total = r.started.elapsed().as_secs_f32();
            spans.push(Span::styled(format!("  {total:.1}s"), theme::faint()));
            if r.phase == Phase::Writing && total > 0.5 {
                spans.push(Span::styled(format!(" · {:.0} chars/s", r.chars as f32 / total), theme::faint()));
            }
        }
        spans.push(Span::styled("  esc to interrupt", theme::faint()));
        Line::from(spans)
    }
}
