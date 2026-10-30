//! Background jobs on screen: a line in the transcript when one starts and
//! when it ends, and a count in the status line while any run. A job's
//! output is not drawn here; the model gets it (or `/status` and the journal
//! have it), so the screen only says that something is going on.

use super::app::App;
use super::theme;
use crate::event::Event;
use crate::jobs::Status;
use ratatui::style::Style;
use ratatui::text::Span;

impl App {
    pub(super) fn on_job(&mut self, ev: &Event) {
        match ev {
            Event::JobStart(s) => {
                let (id, label) = (&s.id, &s.label);
                self.jobs_running += 1;
                self.notice(
                    theme::FAINT,
                    format!("◆ job {id} started · {}", crate::llm::truncate(&label.replace('\n', " ⏎ "), 80)),
                );
            }
            Event::JobEnd(j) => {
                let (id, status, ms) = (&j.id, j.status, j.ms);
                self.jobs_running = self.jobs_running.saturating_sub(1);
                let color = match status {
                    Status::Completed => theme::GREEN,
                    Status::Failed | Status::Timeout => theme::RED,
                    Status::Cancelled | Status::Running => theme::YELLOW,
                };
                self.notice(color, format!("◆ job {id} {} · {:.1}s", status.label(), ms as f64 / 1000.0));
            }
            _ => {}
        }
    }

    /// The status line's note while jobs run.
    pub(super) fn jobs_span(&self, phase: f32) -> Option<Span<'static>> {
        (self.jobs_running > 0).then(|| {
            let s = if self.jobs_running == 1 { "" } else { "s" };
            Span::styled(
                format!("◆ {} job{s} running  ", self.jobs_running),
                Style::new().fg(theme::pulse(phase, theme::FAINT, theme::ACCENT2)),
            )
        })
    }
}
