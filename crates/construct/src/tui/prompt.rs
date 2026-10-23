//! Drawing a permission prompt: a rounded panel that takes over the bottom of
//! the screen (activity line, input and all) until the question is answered.
//!
//! The design choice: it must be impossible to miss and impossible to mistake
//! for output. The border breathes between orange and amber, the title is a
//! filled badge naming who is asking (main agent in blue, a subagent in
//! lavender), the thing being asked about is the brightest text in the panel,
//! and the choices are colored by consequence: green to let it through once or
//! for a while, amber for what is written to config, red for no. The selected
//! row is a filled bar, so the default is visible before any key is pressed.

use super::ask::{Choice, Prompt};
use super::theme;
use crate::permission::{Access, Request};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

const SELECTED_BG: Color = Color::Rgb(44, 48, 72);
const BADGE_FG: Color = Color::Rgb(26, 27, 38);
const PATHS_SHOWN: usize = 4;

/// `s` cut to `w` columns, keeping its end (the part of a path that matters).
fn tail(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut kept: Vec<char> = vec![];
    let mut cw = 1;
    for c in s.chars().rev() {
        let x = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cw + x > w {
            break;
        }
        kept.push(c);
        cw += x;
    }
    format!("…{}", kept.into_iter().rev().collect::<String>())
}

fn head(s: &str, w: usize) -> String {
    super::view::fit(s, w)
}

fn agent_of(p: &Prompt) -> &str {
    match &p.request {
        Request::Path(r) => &r.agent,
        Request::Tool(t) => &t.agent,
    }
}

fn label(p: &Prompt, c: Choice) -> (String, Color) {
    let (agent, kind) = match &p.request {
        Request::Path(r) => (r.agent.as_str(), None),
        Request::Tool(t) => (t.agent.as_str(), t.kind.as_deref()),
    };
    let saves = if agent.is_empty() { "config.toml".to_string() } else { format!("[agents.{agent}] in config.toml") };
    let text = match (&p.request, c) {
        (Request::Path(r), Choice::Once) if r.rerun => "Allow and run the command again".into(),
        (Request::Path(_), Choice::Once) => "Allow once".into(),
        (Request::Path(_), Choice::Session) => "Allow for this session".into(),
        (Request::Path(_), Choice::Always) => format!("Always allow · saved to {saves}"),
        (Request::Tool(_), Choice::Once) => "Allow this call".into(),
        (Request::Tool(_), Choice::Session) => match kind {
            Some(k) => format!("Allow {k} calls from {agent} this session"),
            None => "Allow this call for this session".into(),
        },
        (Request::Tool(_), Choice::Always) => {
            format!("Always allow {} from {agent} · saved to {saves}", kind.unwrap_or("these"))
        }
        (_, Choice::Deny) => "Deny".into(),
    };
    let color = match c {
        Choice::Once | Choice::Session => theme::GREEN,
        Choice::Always => theme::YELLOW,
        Choice::Deny => theme::RED,
    };
    (text, color)
}

/// What the panel says, one line each, for a panel `w` columns wide inside.
fn body(p: &Prompt, w: usize) -> Vec<Line<'static>> {
    let strong = Style::new().fg(Color::Rgb(235, 238, 255)).add_modifier(Modifier::BOLD);
    let mut out = vec![Line::default()];
    match &p.request {
        Request::Path(r) => {
            let shell = r.rerun;
            let wants = match r.access {
                Access::Read => "read from",
                Access::Write => "write to",
            };
            let intro = if shell {
                format!("A shell command was blocked by the sandbox. It tried to {wants}:")
            } else {
                format!("{} is blocked by the sandbox. It needs to {wants}:", r.tool)
            };
            out.push(Line::from(Span::styled(intro, Style::new().fg(theme::DIM))));
            out.push(Line::default());
            let shown =
                if p.folder { vec![p.folder_offered().unwrap_or_default().to_string()] } else { r.paths.clone() };
            for path in shown.iter().take(PATHS_SHOWN) {
                out.push(Line::from(vec![
                    Span::styled("  ▸ ", Style::new().fg(theme::ORANGE)),
                    Span::styled(tail(path, w.saturating_sub(4)), strong.fg(theme::CYAN)),
                ]));
            }
            if shown.len() > PATHS_SHOWN {
                out.push(Line::from(Span::styled(
                    format!("    … and {} more", shown.len() - PATHS_SHOWN),
                    theme::dim(),
                )));
            }
            if p.folder_offered().is_some() {
                let (on, off) = (Style::new().fg(theme::CYAN).add_modifier(Modifier::BOLD), theme::faint());
                out.push(Line::default());
                out.push(Line::from(vec![
                    Span::styled("  scope  ", theme::dim()),
                    Span::styled(if p.folder { "○ this path" } else { "● this path" }, if p.folder { off } else { on }),
                    Span::raw("   "),
                    Span::styled(
                        if p.folder { "● its whole folder" } else { "○ its whole folder" },
                        if p.folder { on } else { off },
                    ),
                    Span::styled("   tab to switch", theme::faint()),
                ]));
            }
            if !r.detail.is_empty() {
                out.push(Line::default());
                out.push(Line::from(vec![
                    Span::styled("  $ ", Style::new().fg(theme::DIM)),
                    Span::styled(head(&r.detail.replace('\n', " ⏎ "), w.saturating_sub(4)), theme::dim()),
                ]));
            }
            if shell {
                out.push(Line::from(Span::styled(
                    "  ↻ allowing it runs the command again",
                    Style::new().fg(theme::YELLOW),
                )));
            }
        }
        Request::Tool(t) => {
            let kind = t.kind.as_deref().map(|k| format!(" ({k})")).unwrap_or_default();
            out.push(Line::from(Span::styled(
                format!("{} wants to run a tool call{kind}:", t.agent),
                Style::new().fg(theme::DIM),
            )));
            out.push(Line::default());
            for l in t.title.lines().take(3) {
                out.push(Line::from(vec![
                    Span::styled("  ▸ ", Style::new().fg(theme::ORANGE)),
                    Span::styled(head(l, w.saturating_sub(4)), strong),
                ]));
            }
        }
    }
    out.push(Line::default());
    for (i, c) in p.choices().into_iter().enumerate() {
        let (text, color) = label(p, c);
        let selected = i == p.selected;
        let bg = if selected { SELECTED_BG } else { Color::Reset };
        let mark = if selected { "❯ " } else { "  " };
        let key = format!("{} ", c.key());
        let used = 2 + 2 + text.width() + key.width();
        let mut text_style = Style::new().fg(color).bg(bg);
        if selected {
            text_style = text_style.add_modifier(Modifier::BOLD);
        }
        out.push(Line::from(vec![
            Span::styled(" ", Style::new().bg(bg)),
            Span::styled(mark, Style::new().fg(color).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(text, text_style),
            Span::styled(" ".repeat(w.saturating_sub(used)), Style::new().bg(bg)),
            Span::styled(key, Style::new().fg(theme::DIM).bg(bg)),
        ]));
    }
    out
}

/// Rows the panel needs at `width`, borders included.
pub(super) fn height(p: &Prompt, width: u16) -> u16 {
    body(p, (width as usize).saturating_sub(4)).len() as u16 + 2
}

pub(super) fn render(f: &mut Frame, area: Rect, p: &Prompt, waiting: usize, phase: f32) {
    let edge = theme::pulse(phase, theme::ORANGE, theme::YELLOW);
    let (who, who_color) = match agent_of(p) {
        "" => ("main agent".to_string(), theme::ACCENT),
        a => (format!("subagent · {a}"), theme::ACCENT2),
    };
    let badge = Span::styled(" ⚠ PERMISSION ", Style::new().fg(BADGE_FG).bg(edge).add_modifier(Modifier::BOLD));
    let title = Line::from(vec![
        badge,
        Span::raw(" "),
        Span::styled(who, Style::new().fg(who_color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ]);
    let mut hint = String::from(" ↑↓ select · enter confirm");
    if p.folder_offered().is_some() {
        hint.push_str(" · tab scope");
    }
    hint.push_str(" · esc interrupt ");
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(edge))
        .padding(Padding::horizontal(1))
        .title_top(title)
        .title_bottom(Line::from(Span::styled(hint, theme::dim())));
    if waiting > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(format!(" +{waiting} waiting "), Style::new().fg(theme::YELLOW))).right_aligned(),
        );
    }
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(body(p, inner.width as usize)), inner);
}
