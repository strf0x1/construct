//! The splash: what an empty transcript shows — a three-row heavy geometric
//! `CONSTRUCT` wordmark (one line on a narrow terminal) centered optically in
//! the viewport with a subtle divider rule, the session's model, endpoint and
//! router underneath, and key hints. With motion on, `motion::splash` runs a
//! luminous beam trace-in and specular sheen over the parts recorded in
//! `Transcript::places`; with motion off, the static gradient wordmark shows at once.

use super::App;
use crate::motion::Place;
use crate::motion::splash::{HINTS, ROWS, RULE, TAGLINE, TITLE};
use crate::theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// The title in heavy geometric box-drawing letters, three rows tall.
const LOGO: [&str; 3] = [
    "┏━━ ┏━┓ ┏┓┃ ┏━━ ━┳━ ┏━┓ ┃ ┃ ┏━━ ━┳━",
    "┃   ┃ ┃ ┃┗┫ ┗━┓  ┃  ┣┳┛ ┃ ┃ ┃    ┃ ",
    "┗━━ ┗━┛ ┃ ┃ ━━┛  ┃  ┃┗━ ┗━┛ ┗━━  ┃ ",
];
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
    pub(super) fn splash(&self, width: u16, height: u16, phase: f32) -> (Vec<Line<'static>>, Vec<Place>) {
        let w = width as usize;
        let wide = w >= LOGO_W + 4;
        let title: &[&str] = if wide { &LOGO } else { &["◆ construct"] };
        let title_w = if wide { LOGO_W } else { 11 };

        // Session info rows: align their block cleanly beneath the logo.
        let mut info = vec![
            ("model", self.cfg.model.clone()),
            ("endpoint", self.cfg.endpoint().base_url),
            ("router", self.cfg.router.mode.clone()),
        ];
        if !self.session.is_empty() {
            info.push(("session", self.session.clone()));
        }
        let info_w = info.iter().map(|(_, v)| 9 + v.width()).max().unwrap_or(0).max(title_w);
        let col_pad = |block_w: usize| {
            if w > block_w + 4 { (w - block_w) / 2 } else { 2 }
        };
        let logo_pad = col_pad(title_w);
        let info_pad = col_pad(info_w.min(w.saturating_sub(4)));

        // Content height: title + gap + tagline + rule + gap + info + gap + hints.
        let content_h = title.len() + 1 + 1 + 1 + 1 + info.len() + 1 + 1;
        // Optical vertical center (slightly above geometric midpoint).
        let top = (height as usize).saturating_sub(content_h) * 2 / 5;
        let mut lines = vec![Line::default(); top];
        let mut parts = [(0usize, 0usize, 0usize); 5];

        // 1. Wordmark
        parts[TITLE] = (lines.len(), title.len(), logo_pad);
        for row in title {
            let n = row.chars().count().max(1) as f32;
            let mut t = vec![Span::raw(" ".repeat(logo_pad))];
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
        lines.push(Line::default());

        // 2. Tagline
        let tag_text = "◆ lean · fast · learns as it goes";
        let tag_pad = col_pad(tag_text.width());
        parts[TAGLINE] = (lines.len(), 1, tag_pad);
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(tag_pad)),
            Span::styled("◆ ", Style::new().fg(theme::ACCENT2)),
            Span::styled("lean · fast · learns as it goes", Style::new().fg(theme::SOFT)),
        ]));

        // 3. Subtle divider rule matching the wordmark width
        let rule_w = title_w.min(w.saturating_sub(4));
        let rule_pad = col_pad(rule_w);
        parts[RULE] = (lines.len(), 1, rule_pad);
        lines.push(Line::from(vec![Span::raw(" ".repeat(rule_pad)), Span::styled("─".repeat(rule_w), theme::faint())]));
        lines.push(Line::default());

        // 4. Session info rows
        let first = lines.len();
        for (k, v) in info {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(info_pad)),
                Span::styled(format!("{k:<9}"), theme::faint()),
                Span::styled(v, theme::dim()),
            ]));
        }
        parts[ROWS] = (first, lines.len() - first, info_pad);
        lines.push(Line::default());

        // 5. Key hints
        let mut hints_inner = Vec::new();
        for (i, (key, what)) in HINTS_KEYS.iter().enumerate() {
            if i > 0 {
                hints_inner.push(Span::styled(" · ", theme::faint()));
            }
            hints_inner.push(Span::styled(*key, theme::accent()));
            hints_inner.push(Span::styled(format!(" {what}"), theme::faint()));
        }
        let hints_w: usize = hints_inner.iter().map(|s| s.content.width()).sum();
        let hints_pad = col_pad(hints_w);
        parts[HINTS] = (lines.len(), 1, hints_pad);
        let mut hints = vec![Span::raw(" ".repeat(hints_pad))];
        hints.extend(hints_inner);
        lines.push(Line::from(hints));

        let places = parts
            .iter()
            .map(|&(start, len, left)| {
                let right = lines[start..start + len].iter().map(Line::width).max().unwrap_or(0);
                Place { left: left as u16, start, len, width: right.saturating_sub(left) as u16 }
            })
            .collect();
        (lines, places)
    }
}
