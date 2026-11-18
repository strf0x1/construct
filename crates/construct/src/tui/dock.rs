//! The dock: where a running delegated child shows.
//!
//! The dock is a row per running child between the transcript and the
//! activity line, so a background child is on screen however far the
//! transcript has scrolled; it takes no rows when nothing runs. The peek
//! panel is `peek.rs`.
//!
//! The design choice: the rows (and the panel) are drawn from
//! `children::Dock`, a small read-model of the event stream, and take their
//! rows from the layout (`view::draw`) instead of the transcript, so nothing
//! in the block list changes and nothing here is journaled.

use super::app::{App, Kind};
use super::args::primary_arg;
use super::children::Child;
use super::theme;
use super::view::fit;
use crate::event::Event;
use crate::jobs::Started;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
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
}

pub(super) fn elapsed(c: &Child) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

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
}
