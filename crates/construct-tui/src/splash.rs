//! The splash: what an empty transcript shows, the title shimmering in
//! once and the session's model, endpoint and router underneath. With motion
//! on, the title skips the shimmer and `motion::splash` runs its intro over
//! the parts recorded in `App::rows`.

use super::app::App;
use super::motion::splash::{HINTS, ROWS, TAGLINE, TITLE};
use super::theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

impl App {
    pub(super) fn splash(&mut self, area: Rect, phase: f32) -> Vec<Line<'static>> {
        let mut lines = vec![];
        let top = area.height.saturating_sub(9) / 3;
        for _ in 0..top {
            lines.push(Line::default());
        }
        let title = "◆ construct";
        let mut t = vec![Span::raw("  ")];
        if phase < 1.6 && !self.motion.on() {
            t.extend(theme::shimmer(title, phase, theme::ACCENT, Color::Rgb(235, 235, 255)));
        } else {
            t.extend(title.chars().enumerate().map(|(i, c)| {
                Span::styled(
                    c.to_string(),
                    Style::new()
                        .fg(theme::lerp(theme::ACCENT, theme::ACCENT2, i as f32 / 10.0))
                        .add_modifier(Modifier::BOLD),
                )
            }));
        }
        let mut parts = [(0, 0); 4];
        parts[TITLE] = (lines.len(), 1);
        lines.push(Line::from(t));
        parts[TAGLINE] = (lines.len(), 1);
        lines.push(Line::from(Span::styled("  lean · fast · learns as it goes", theme::dim())));
        lines.push(Line::default());
        let row = |k: &str, v: String| {
            Line::from(vec![Span::styled(format!("  {k:<9}"), theme::faint()), Span::styled(v, theme::dim())])
        };
        let first = lines.len();
        lines.push(row("model", self.cfg.model.clone()));
        lines.push(row("endpoint", self.cfg.endpoint().base_url));
        lines.push(row("router", self.cfg.router.mode.clone()));
        if !self.session.is_empty() {
            lines.push(row("session", self.session.clone()));
        }
        parts[ROWS] = (first, lines.len() - first);
        lines.push(Line::default());
        parts[HINTS] = (lines.len(), 1);
        lines.push(Line::from(Span::styled(
            "  enter send · alt+enter newline · esc interrupt · ctrl+o expand · pgup/pgdn scroll · /help",
            theme::faint(),
        )));
        self.rows = parts.to_vec();
        lines
    }
}
