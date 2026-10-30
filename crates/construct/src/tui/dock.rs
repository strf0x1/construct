//! The dock and the peek panel: where a running delegated child shows.
//!
//! The dock is a row per running child between the transcript and the
//! activity line, so a background child is on screen however far the
//! transcript has scrolled; it takes no rows when nothing runs. `ctrl+t`
//! opens the peek panel over the transcript: a bordered popup with the
//! child's last steps and the words it is saying, one tab per child.
//!
//! The design choice: both are drawn from `children::Dock`, a small
//! read-model of the event stream, and take their rows from the layout
//! (`view::draw`) instead of the transcript, so nothing in the block list
//! changes and nothing here is journaled.

use super::app::{App, Kind};
use super::args::primary_arg;
use super::children::{Child, Entry};
use super::view::fit;
use super::{markdown, theme};
use crate::event::Event;
use crate::jobs::Started;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

/// Most rows the dock takes; past it the last one says how many more.
const ROWS: usize = 3;

impl App {
    /// An event from the child a delegate call is running.
    pub(super) fn on_child(&mut self, call: &str, ev: Event) {
        let detail = match &ev {
            Event::ToolStart { name, args, .. } => {
                let d = self.catalog.as_ref().map(|c| c.display(name)).unwrap_or_default();
                primary_arg(d.primary_arg, args)
            }
            Event::ToolEnd { .. } | Event::Text { .. } => String::new(),
            Event::TurnEnd { .. } => return self.dock.finish(call),
            _ => return,
        };
        if !self.dock.has(call) {
            let label = self.child_label(call);
            self.dock.add(call, label);
        }
        let Some(c) = self.dock.child_mut(call) else { return };
        match ev {
            Event::ToolStart { id, name, .. } => c.tool_start(id, name, detail),
            Event::ToolEnd { id, ok, ms, .. } => c.tool_end(&id, ok, ms),
            Event::Text { text } => c.say(&text),
            _ => {}
        }
    }

    /// The agent the call names, as its transcript line shows it.
    fn child_label(&self, call: &str) -> String {
        let call_block = self.blocks.iter().rev().find(|b| b.kind == Kind::Tool && b.id == call);
        call_block.map(|b| b.detail.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| "agent".into())
    }

    /// The delegate call ended: a foreground child ended with it, a
    /// background one carries on until its job does.
    pub(super) fn end_call(&mut self, call: &str) {
        if !self.dock.is_job(call) {
            self.dock.finish(call);
        }
    }

    /// A job started: if it is a delegated child, it gets a row now.
    pub(super) fn job_started(&mut self, s: &Started) {
        if s.kind != crate::jobs::Kind::Delegate {
            return;
        }
        let label = self.child_label(&s.call_id);
        self.dock.add(&s.call_id, label);
        if let Some(c) = self.dock.child_mut(&s.call_id) {
            c.job = Some(s.id.clone());
        }
    }

    /// The rows between the transcript and the activity line.
    pub(super) fn dock_lines(&self, width: usize, phase: f32) -> Vec<Line<'static>> {
        let n = self.dock.children.len();
        let shown = if n > ROWS { ROWS - 1 } else { n };
        let mut out: Vec<Line<'static>> =
            self.dock.children.iter().take(shown).enumerate().map(|(i, c)| row(c, width, phase, i == 0)).collect();
        if n > shown {
            out.push(Line::from(Span::styled(format!("   +{} more", n - shown), theme::faint())));
        }
        out
    }

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
            .title_bottom(Line::from(Span::styled(" esc close · ←→ switch · ↑↓ scroll ", theme::faint())));
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

fn elapsed(c: &Child) -> String {
    let s = c.started.elapsed().as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m{:02}s", s / 60, s % 60) }
}

/// One child: `⠧ j1  scout  shell sleep 6 · "words so far"      4s  ctrl+t peek`.
fn row(c: &Child, width: usize, phase: f32, hint: bool) -> Line<'static> {
    let id = c.job.as_deref().unwrap_or("·");
    let head = format!(" {} {id:<3} ", theme::spinner(phase));
    let name = format!("{} ", crate::llm::truncate(&c.label, 16));
    let tail = format!(" {}{} ", elapsed(c), if hint && width >= 70 { "  ctrl+t peek" } else { "" });
    let mut what = c.last_tool().map(|(n, d)| format!("{n} {d}")).unwrap_or_default();
    if !c.said_line().is_empty() {
        if !what.is_empty() {
            what.push_str(" · ");
        }
        what.push_str(c.said_line());
    }
    if what.is_empty() {
        what = "starting…".into();
    }
    let room = width.saturating_sub(head.width() + name.width() + tail.width());
    let what = fit(&what, room);
    let pad = room.saturating_sub(what.width());
    Line::from(vec![
        Span::styled(head, Style::new().fg(theme::ACCENT2)),
        Span::styled(name, theme::dim()),
        Span::styled(what, theme::faint()),
        Span::raw(" ".repeat(pad)),
        Span::styled(tail, theme::faint()),
    ])
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

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_dock_takes_a_row_per_child_up_to_three_and_none_when_idle() {
        let mut app = App::new(Config::default());
        assert!(app.dock_lines(100, 0.0).is_empty());
        for i in 0..5 {
            app.dock.add(&format!("c{i}"), "scout".into());
        }
        let rows = app.dock_lines(100, 0.0);
        assert_eq!(rows.len(), ROWS);
        assert_eq!(text(&rows[ROWS - 1]).trim(), "+3 more");
        assert!(text(&rows[0]).contains("ctrl+t peek") && !text(&rows[1]).contains("ctrl+t"));
        assert!(rows.iter().all(|r| text(r).width() <= 100));
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
}
