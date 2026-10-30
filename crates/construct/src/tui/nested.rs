//! A delegated child's progress on screen: one or two faint lines under the
//! delegate call (or, for a background child, under its `◆ job … started`
//! line) saying what the child is doing now, gone when it ends.
//!
//! The design choice: the lines are a `Notice` block of their own, filed
//! right after the call's block and found again by id (`child:<call id>`), so
//! nothing here touches how blocks are drawn. The block keeps the child's
//! current tool in `title` and its latest text line in `detail`, and
//! `text` is what the two compose to. Only the last tool stays up between
//! tools, so the block grows from one line to two once and never flickers.

use super::app::{App, Block, Kind};
use super::args::primary_arg;
use super::theme;
use crate::event::Event;

/// Longest a line gets before the transcript would wrap it.
const LINE: usize = 70;

fn block_id(call: &str) -> String {
    format!("child:{call}")
}

/// Id of the `◆ job … started` line, with the call that made the job in `detail`.
pub(super) fn job_block_id(job: &str) -> String {
    format!("job:{job}")
}

impl App {
    /// An event from the child a delegate call is running.
    pub(super) fn on_child(&mut self, call: &str, ev: Event) {
        match ev {
            Event::ToolStart { name, args, .. } => {
                let d = self.catalog.as_ref().map(|c| c.display(&name)).unwrap_or_default();
                let tool = format!("{name} {}", primary_arg(d.primary_arg, &args));
                self.progress(call, |title, said| {
                    *title = crate::llm::truncate(tool.trim(), LINE);
                    said.clear();
                });
            }
            Event::Text { text } => self.progress(call, |_, said| {
                said.push_str(&text);
                // Only the tail can be the last line; keep the string small.
                if let Some((cut, _)) = said.char_indices().rev().nth(4 * LINE) {
                    said.drain(..cut);
                }
            }),
            Event::TurnEnd { .. } => self.end_child(call),
            _ => {}
        }
    }

    /// Drop the progress lines of a child that ended. A background child
    /// outlives its delegate call, so that call's end does not count.
    pub(super) fn end_child(&mut self, call: &str) {
        let id = block_id(call);
        if let Some(i) = self.blocks.iter().rposition(|b| b.id == id) {
            self.blocks.remove(i);
            self.cache.clear();
        }
    }

    /// The delegate call ended: a foreground child ended with it.
    pub(super) fn end_call(&mut self, call: &str) {
        if !self.is_job(call) {
            self.end_child(call);
        }
    }

    /// A job ended: its child (if it had one) is done.
    pub(super) fn end_job(&mut self, job: &str) {
        let id = job_block_id(job);
        if let Some(call) = self.blocks.iter().find(|b| b.id == id).map(|b| b.detail.clone()) {
            self.end_child(&call);
        }
    }

    fn is_job(&self, call: &str) -> bool {
        self.blocks.iter().any(|b| b.id.starts_with("job:") && b.detail == call)
    }

    /// Change the child's tool and text lines, creating the block under the
    /// call on first use. The block is touched only when what it says changed:
    /// the model streams a word at a time and most words stay on the same line.
    fn progress(&mut self, call: &str, f: impl FnOnce(&mut String, &mut String)) {
        let id = block_id(call);
        let at = self.blocks.iter().rposition(|b| b.id == id);
        let (mut title, mut said) =
            at.map(|i| (self.blocks[i].title.clone(), self.blocks[i].detail.clone())).unwrap_or_default();
        f(&mut title, &mut said);
        let lines: Vec<String> =
            [title.clone(), last_line(&said)].iter().filter(|l| !l.is_empty()).map(|l| format!("  ↳ {l}")).collect();
        let text = lines.join("\n");
        let i = match at {
            Some(i) => i,
            None if text.is_empty() => return,
            None => {
                // After the call's block, or the job line if it has one.
                let Some(after) = self.blocks.iter().rposition(|b| {
                    (b.kind == Kind::Tool && b.id == call) || (b.id.starts_with("job:") && b.detail == call)
                }) else {
                    return;
                };
                let mut b = Block::new(Kind::Notice(theme::FAINT), "");
                b.id = id;
                self.blocks.insert(after + 1, b);
                self.cache.clear();
                after + 1
            }
        };
        let b = &mut self.blocks[i];
        (b.title, b.detail) = (title, said);
        if b.text != text {
            b.text = text;
            b.version += 1;
        }
    }
}

/// The last non-empty line of what the child has said.
fn last_line(s: &str) -> String {
    let line = s.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    crate::llm::truncate(line, LINE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn child(app: &mut App, ev: Event) {
        app.on_event(Event::Child { id: "c1".into(), inner: Box::new(ev) });
    }

    fn tool_start(app: &mut App, id: &str) {
        app.on_event(Event::ToolStart { id: id.into(), name: "delegate".into(), args: "{}".into() });
    }

    fn shown(app: &App) -> Vec<&str> {
        app.blocks.iter().filter(|b| b.id == "child:c1").map(|b| b.text.as_str()).collect()
    }

    #[test]
    fn a_foreground_child_shows_its_tool_and_words_and_goes_with_the_call() {
        let mut app = App::new(Config::default());
        tool_start(&mut app, "c1");
        child(&mut app, Event::ToolStart { id: "t".into(), name: "shell".into(), args: r#"{"command":"ls"}"#.into() });
        assert_eq!(app.blocks[1].id, "child:c1", "filed right after the call");
        child(&mut app, Event::Text { text: "one\ntwo".into() });
        child(&mut app, Event::Text { text: " three".into() });
        assert_eq!(shown(&app).len(), 1);
        assert!(shown(&app)[0].ends_with("↳ two three"), "{:?}", shown(&app));
        // The next tool replaces what was said.
        child(&mut app, Event::ToolStart { id: "u".into(), name: "read".into(), args: "{}".into() });
        assert_eq!(shown(&app)[0].lines().count(), 1);
        app.on_event(Event::ToolEnd {
            id: "c1".into(),
            name: "delegate".into(),
            ok: true,
            output: String::new(),
            ms: 1,
            kind: crate::tools::OutputKind::Plain,
            data: None,
            code: None,
        });
        assert!(shown(&app).is_empty());
    }

    #[test]
    fn a_background_child_outlives_its_call_and_goes_with_its_job() {
        let mut app = App::new(Config::default());
        tool_start(&mut app, "c1");
        let started = crate::jobs::Started {
            id: "j1".into(),
            kind: crate::jobs::Kind::Delegate,
            label: "scout".into(),
            call_id: "c1".into(),
        };
        app.on_event(Event::JobStart(started));
        child(&mut app, Event::Text { text: "working".into() });
        let at = app.blocks.iter().position(|b| b.id == "child:c1").unwrap_or_default();
        assert_eq!(app.blocks[at - 1].id, "job:j1", "filed under the job's line");
        app.end_call("c1");
        assert_eq!(shown(&app).len(), 1, "the delegate call ending is not the child ending");
        app.end_job("j1");
        assert!(shown(&app).is_empty());
    }
}
