//! The peek panel: `ctrl+t` opens a popup over the transcript with the
//! selected child's last steps and the words it is saying, a tab per child
//! and a scrollbar; `ctrl+x` then `y` stops the child.
//!
//! The design choice: stopping a child takes two deliberate keys, the
//! second within seconds of the first, and any other key in between keeps it
//! running, so a stray keypress never kills a job. The panel takes every key
//! while it is open (but never ctrl+c or ctrl+d), so it can't be typed into.

use super::app::App;
use super::children::{Child, Entry};
use super::dock::elapsed;
use super::view::fit;
use super::{markdown, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// How long a cancel waits for its `y`.
const CONFIRM: Duration = Duration::from_secs(4);

impl App {
    /// `ctrl+t` opens and closes the peek panel, and while it is open it
    /// takes the keys (but never ctrl+c or ctrl+d). Returns whether it did.
    pub(super) fn peek_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('t') {
            if self.dock.peek.is_some() {
                self.dock.peek = None;
            } else if self.dock.active() {
                self.dock.peek = Some(Default::default());
            } else {
                self.toast = Some(("no agents running".into(), theme::FAINT, Instant::now()));
            }
            return true;
        }
        let n = self.dock.children.len();
        let Some(p) = self.dock.peek.as_mut() else { return false };
        if ctrl && matches!(k.code, KeyCode::Char('c' | 'd')) {
            return false;
        }
        // Stopping a child is two deliberate steps: ctrl+x, then y within
        // seconds. Any other key in between keeps it running.
        let armed = p.armed.take().is_some_and(|t| t.elapsed() < CONFIRM);
        if armed {
            if k.code == KeyCode::Char('y') {
                self.cancel_selected();
            }
            return true;
        }
        if ctrl && k.code == KeyCode::Char('x') {
            p.armed = Some(Instant::now());
            return true;
        }
        let switch = |p: &mut super::children::Peek, to: usize| (p.sel, p.back) = (to, 0);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.dock.peek = None,
            KeyCode::Left | KeyCode::BackTab => switch(p, (p.sel + n - 1) % n),
            KeyCode::Right | KeyCode::Tab => switch(p, (p.sel + 1) % n),
            KeyCode::Up => p.back += 1,
            KeyCode::Down => p.back = p.back.saturating_sub(1),
            KeyCode::PageUp => p.back += 5,
            KeyCode::PageDown => p.back = p.back.saturating_sub(5),
            _ => {}
        }
        true
    }

    /// Stop the child the panel is on: a background one is cancelled as the
    /// job it is (journaled `cancelled`, wakes nobody); a foreground one
    /// ends with the turn it belongs to, so that is interrupted.
    fn cancel_selected(&mut self) {
        let Some(c) = self.dock.peek.as_ref().and_then(|p| self.dock.children.get(p.sel)) else { return };
        let (label, job) = (c.label.clone(), c.job.clone());
        let msg = match (job, &self.dock.jobs) {
            (Some(id), Some(jobs)) => {
                jobs.cancel(&id);
                format!("cancelling {id} ({label})")
            }
            _ => {
                if let Some(x) = &self.canceller {
                    x.send_replace(true);
                }
                format!("interrupting the turn ({label})")
            }
        };
        self.toast = Some((msg, theme::YELLOW, Instant::now()));
    }

    /// The peek panel, anchored bottom-right of `area` (the transcript), so
    /// it rises out of the dock.
    pub(super) fn render_peek(&self, f: &mut Frame, area: Rect, phase: f32) {
        let Some(p) = &self.dock.peek else { return };
        let Some(c) = self.dock.children.get(p.sel) else { return };
        let (w, h) = (area.width.min(68), area.height.min(16));
        if w < 24 || h < 5 {
            return;
        }
        let rect = Rect::new(area.x + area.width - w, area.y + area.height - h, w, h);
        f.render_widget(Clear, rect);
        let title = Line::from(vec![
            Span::styled(format!(" {} ", c.label), Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} · {} ", c.job.as_deref().unwrap_or("foreground"), elapsed(c)), theme::faint()),
        ]);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::ACCENT2))
            .title(title)
            .title_bottom(footer(c, p.armed));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let body = if self.dock.children.len() > 1 {
            let [tabs, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
            let titles =
                self.dock.children.iter().map(|c| format!(" {} {} ", c.job.as_deref().unwrap_or("·"), c.label));
            let on = Style::new().fg(theme::ACCENT2).add_modifier(Modifier::BOLD);
            f.render_widget(
                Tabs::new(titles).select(p.sel).style(theme::faint()).highlight_style(on).divider("│"),
                tabs,
            );
            body
        } else {
            inner
        };
        let lines = peek_lines(c, body.width.saturating_sub(1) as usize, phase);
        let rows = body.height as usize;
        let max_back = lines.len().saturating_sub(rows);
        let end = lines.len() - p.back.min(max_back);
        let start = end.saturating_sub(rows);
        let text = Rect { width: body.width.saturating_sub(1), ..body };
        f.render_widget(Paragraph::new(lines[start..end].to_vec()), text);
        if max_back > 0 {
            let mut bar = ScrollbarState::new(lines.len()).viewport_content_length(rows).position(start);
            f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight), body, &mut bar);
        }
    }
}

/// The panel's bottom edge: the keys, or the question a cancel is waiting on.
fn footer(c: &Child, armed: Option<Instant>) -> Line<'static> {
    if armed.is_some_and(|t| t.elapsed() < CONFIRM) {
        let which = c.job.as_deref().map_or(c.label.clone(), |j| format!("{j} {}", c.label));
        return Line::from(Span::styled(
            format!(" cancel {which}? press y · anything else keeps it "),
            Style::new().fg(theme::RED).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(Span::styled(" esc close · ←→ switch · ↑↓ scroll · ctrl+x cancel ", theme::faint()))
}

/// The panel's body: the child's steps, then what it is saying now.
fn peek_lines(c: &Child, w: usize, phase: f32) -> Vec<Line<'static>> {
    let mut out = vec![];
    for e in &c.log {
        match e {
            Entry::Tool { name, detail, done, .. } => {
                let (icon, color) = match done {
                    None => (theme::spinner(phase), theme::YELLOW),
                    Some((true, _)) => ("✓", theme::GREEN),
                    Some((false, _)) => ("✗", theme::RED),
                };
                let time = match done {
                    Some((_, ms)) if *ms >= 1000 => format!(" {:.1}s", *ms as f32 / 1000.0),
                    Some((_, ms)) => format!(" {ms}ms"),
                    None => String::new(),
                };
                let room = w.saturating_sub(4 + name.width() + time.width());
                out.push(Line::from(vec![
                    Span::styled(format!(" {icon} "), Style::new().fg(color)),
                    Span::styled(name.clone(), Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
                    Span::raw(" "),
                    Span::styled(fit(detail, room), theme::dim()),
                    Span::styled(time, theme::faint()),
                ]));
            }
            Entry::Said(t) => out.extend(said(t, w, theme::dim())),
        }
    }
    out.extend(said(c.said.trim(), w, Style::new()));
    if out.is_empty() {
        out.push(Line::from(Span::styled(" starting…", theme::faint())));
    }
    out
}

fn said(text: &str, w: usize, style: Style) -> Vec<Line<'static>> {
    let bar = Span::styled(" ▏", theme::faint());
    text.split('\n')
        .filter(|p| !p.trim().is_empty())
        .flat_map(|p| {
            markdown::wrap(vec![Span::styled(p.trim().to_string(), style)], w, vec![bar.clone()], vec![bar.clone()])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ctrl_t() -> KeyEvent {
        key(KeyCode::Char('t'), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_t_opens_the_peek_only_with_a_child_and_it_takes_keys_but_not_ctrl_c() {
        let mut app = App::new(Config::default());
        assert!(app.peek_key(ctrl_t()) && app.dock.peek.is_none(), "nothing running: a toast, no panel");
        assert!(!app.peek_key(key(KeyCode::Char('x'), KeyModifiers::NONE)), "closed: keys are not ours");
        app.dock.add("a", "x".into());
        app.dock.add("b", "y".into());
        assert!(app.peek_key(ctrl_t()) && app.dock.peek.is_some());
        assert!(app.peek_key(key(KeyCode::Right, KeyModifiers::NONE)));
        assert_eq!(app.dock.peek.as_ref().map(|p| p.sel), Some(1));
        assert!(app.peek_key(key(KeyCode::Char('z'), KeyModifiers::NONE)), "open: typing does not reach the input");
        assert!(!app.peek_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), "ctrl+c still interrupts");
        assert!(app.peek_key(key(KeyCode::Esc, KeyModifiers::NONE)) && app.dock.peek.is_none());
    }

    #[test]
    fn a_cancel_needs_ctrl_x_then_y_and_anything_else_disarms_it() {
        let mut app = App::new(Config::default());
        let (tx, mut seen) = tokio::sync::watch::channel(false);
        app.canceller = Some(tx);
        app.dock.add("a", "scout".into());
        app.peek_key(ctrl_t());
        let y = key(KeyCode::Char('y'), KeyModifiers::NONE);
        // A stray y, or ctrl+x then something else then y, cancels nothing.
        app.peek_key(y);
        app.peek_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        app.peek_key(key(KeyCode::Down, KeyModifiers::NONE));
        app.peek_key(y);
        assert!(!seen.has_changed().unwrap_or(false));
        // ctrl+x then y does (a foreground child: the turn is interrupted).
        app.peek_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        app.peek_key(y);
        assert!(*seen.borrow_and_update());
        assert!(app.toast.as_ref().is_some_and(|t| t.0.contains("interrupting")));
    }
}
