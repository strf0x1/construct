//! The splash: what an empty transcript shows, the title in block letters
//! (one line on a narrow terminal) shimmering in once, and the session's
//! model, endpoint and router underneath. With motion
//! on, the title skips the shimmer and `motion::splash` runs its intro over
//! the parts recorded in `App::rows`.

use super::app::App;
use super::motion::Place;
use super::motion::splash::{HINTS, ROWS, TAGLINE, TITLE};
use super::theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The title in block letters, two rows tall, where there is room for it.
const LOGO: [&str; 2] = ["▄▀▀ ▄▀▄ █▄ █ ▄▀▀ ▀█▀ █▀▄ █ █ ▄▀▀ ▀█▀", "▀▄▄ ▀▄▀ █ ▀█ ▄██  █  █▀▄ ▀▄▀ ▀▄▄  █ "];
const LOGO_W: usize = 35;
/// The key hints under the splash: key, then what it does.
const HINTS_KEYS: [(&str, &str); 6] = [
    ("enter", "send"),
    ("alt+enter", "newline"),
    ("esc", "interrupt"),
    ("ctrl+o", "expand"),
    ("pgup/pgdn", "scroll"),
    ("/help", "commands"),
];

impl App {
    pub(super) fn splash(&mut self, area: Rect, phase: f32) -> Vec<Line<'static>> {
        let mut lines = vec![];
        let title: &[&str] = if area.width as usize >= LOGO_W + 4 { &LOGO } else { &["◆ construct"] };
        let top = area.height.saturating_sub(8 + title.len() as u16) / 3;
        for _ in 0..top {
            lines.push(Line::default());
        }
        let mut parts = [(0, 0); 4];
        parts[TITLE] = (lines.len(), title.len());
        for row in title {
            let n = row.chars().count().max(1) as f32;
            let mut t = vec![Span::raw("  ")];
            if phase < 1.6 && !self.motion.on() {
                t.extend(theme::shimmer(row, phase, theme::ACCENT, Color::Rgb(235, 235, 255)));
            } else {
                t.extend(row.chars().enumerate().map(|(i, c)| {
                    let fg = theme::lerp(theme::ACCENT, theme::ACCENT2, i as f32 / n);
                    Span::styled(c.to_string(), Style::new().fg(fg).add_modifier(Modifier::BOLD))
                }));
            }
            lines.push(Line::from(t));
        }
        if title.len() > 1 {
            lines.push(Line::default()); // block letters need air under them
        }
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
        let mut hints = vec![Span::raw("  ")];
        for (i, (key, what)) in HINTS_KEYS.iter().enumerate() {
            if i > 0 {
                hints.push(Span::styled(" · ", theme::faint()));
            }
            hints.push(Span::styled(*key, theme::accent()));
            hints.push(Span::styled(format!(" {what}"), theme::faint()));
        }
        lines.push(Line::from(hints));
        self.rows = parts
            .iter()
            .map(|&(start, len)| {
                let width = lines[start..start + len].iter().map(Line::width).max().unwrap_or(0) as u16;
                Place { start, len, width }
            })
            .collect();
        lines
    }
}
