//! Transcript state: blocks, notices held while a reply streams, per-block
//! line cache, rendered row places for `motion`, viewport scroll, and tool
//! expansion state. `Block` stays private inside this module; callers feed
//! high-level entries and agent events and ask for the visible frame.

mod block;
mod events;
mod tool;

use crate::boxed;
use crate::motion::Place;
use construct_core::tools::OutputKind;
use ratatui::style::Color;
use ratatui::text::Line;
use std::collections::HashMap;
use std::time::Instant;

/// Something appended directly to the transcript by the UI.
pub(crate) enum Item {
    User(String),
    Help(String),
    Boxes {
        max: usize,
        sections: Vec<boxed::Section>,
        note: String,
    },
    Notice(Color, String),
    /// Held while assistant text or reasoning is streaming so it never splits a reply.
    Aside(Color, String),
}

/// How to move the transcript viewport.
pub(crate) enum Scroll {
    Up(usize),
    Down(usize),
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Status {
    Running,
    Ok,
    Err,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Notice(Color),
    /// The `/help` panel; drawn by `help.rs`, so the block carries no text.
    Help,
    /// Rounded boxes (`boxed.rs`) up to this wide, from `Block::boxes`, with
    /// `Block::text` as a dim note underneath.
    Boxes(usize),
}

pub(super) struct Block {
    kind: Kind,
    text: String,
    title: String,
    detail: String,
    status: Status,
    started: Instant,
    ms: u64,
    id: String,
    version: u64,
    /// For tool blocks: lines of output to preview, how to render it, and live tail.
    preview: usize,
    out: OutputKind,
    live: Vec<(String, Instant)>,
    partial: String,
    had_live: bool,
    /// For `Kind::Boxes`: what to draw.
    boxes: Vec<boxed::Section>,
}

impl Block {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            title: String::new(),
            detail: String::new(),
            status: Status::Ok,
            started: Instant::now(),
            ms: 0,
            id: String::new(),
            version: 0,
            preview: 0,
            out: OutputKind::Plain,
            live: Vec::new(),
            partial: String::new(),
            had_live: false,
            boxes: Vec::new(),
        }
    }

    fn running(kind: Kind, text: impl Into<String>) -> Self {
        Self { status: Status::Running, ..Self::new(kind, text) }
    }

    fn tool(id: String, name: String, detail: String, preview: usize) -> Self {
        Self { id, title: name, detail, preview, status: Status::Running, ..Self::new(Kind::Tool, "") }
    }

    fn boxes(max: usize, boxes: Vec<boxed::Section>, note: impl Into<String>) -> Self {
        Self { boxes, ..Self::new(Kind::Boxes(max), note) }
    }

    fn is_streaming_text(&self) -> bool {
        self.status == Status::Running && matches!(self.kind, Kind::Assistant | Kind::Reasoning)
    }

    fn is_running_reasoning(&self) -> bool {
        self.kind == Kind::Reasoning && self.status == Status::Running
    }

    fn is_running_tool(&self) -> bool {
        self.kind == Kind::Tool && self.status == Status::Running
    }

    fn matches_tool(&self, id: &str) -> bool {
        self.kind == Kind::Tool && self.id == id
    }

    fn is_expandable(&self) -> bool {
        matches!(self.kind, Kind::Tool | Kind::Reasoning)
    }

    fn append_text(&mut self, text: &str) {
        self.text.push_str(text);
        self.version += 1;
    }

    fn close_reasoning(&mut self) {
        self.status = Status::Ok;
        self.ms = self.started.elapsed().as_millis() as u64;
        self.version += 1;
    }

    fn push_live(&mut self, chunk: &str) {
        let now = Instant::now();
        for ch in chunk.chars() {
            match ch {
                '\n' => {
                    let line = std::mem::take(&mut self.partial);
                    self.live.push((line, now));
                    if self.live.len() > 32 {
                        let drop = self.live.len() - 32;
                        self.live.drain(..drop);
                    }
                }
                '\r' => self.partial.clear(),
                c => self.partial.push(c),
            }
        }
        self.version += 1;
    }

    fn finish_tool(&mut self, ok: bool, output: String, ms: u64, kind: OutputKind) -> Option<usize> {
        let live_count = self.live.len() + usize::from(!self.partial.is_empty());
        let from_live = (live_count > 0).then(|| live_count.min(tool::LIVE_TAIL));
        self.had_live = live_count > 0;
        self.live.clear();
        self.partial.clear();
        self.status = if ok { Status::Ok } else { Status::Err };
        self.text = output;
        self.ms = ms;
        self.out = kind;
        self.version += 1;
        from_live
    }

    fn finish_history_tool(&mut self, content: String) {
        self.status = if content.starts_with("Error:") { Status::Err } else { Status::Ok };
        self.text = content;
    }

    fn settle_running(&mut self) {
        if self.status == Status::Running {
            self.status = if self.kind == Kind::Tool { Status::Err } else { Status::Ok };
            self.version += 1;
        }
    }

    fn fail_if_running(&mut self) {
        if self.status == Status::Running {
            self.status = Status::Err;
        }
    }
}

#[derive(Default)]
pub(crate) struct Transcript {
    blocks: Vec<Block>,
    /// Notices from beside the turn, held while a reply streams (see `Item::Aside`).
    held: Vec<Block>,
    cache: HashMap<usize, (u16, u64, bool, Vec<Line<'static>>)>,
    /// Where each block landed in the last draw, for `motion` and `toggle_expand`.
    rows: Vec<Place>,
    scroll: usize,
    last_total: usize,
    expand: bool,
}

impl Transcript {
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn clear_cache(&mut self) {
        self.cache.clear();
    }

    pub(crate) fn scroll_by(&mut self, op: Scroll) {
        match op {
            Scroll::Up(n) => self.scroll += n,
            Scroll::Down(n) => self.scroll = self.scroll.saturating_sub(n),
            Scroll::Bottom => self.scroll = 0,
        }
    }

    /// Flip tool/reasoning expansion and return each affected block's previous
    /// body row count so `motion` can ease the transition.
    pub(crate) fn toggle_expand(&mut self) -> Vec<(usize, usize)> {
        let heights: Vec<(usize, usize)> = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_expandable())
            .filter_map(|(i, _)| self.rows.get(i).map(|r| (i, r.len.saturating_sub(1))))
            .collect();
        self.expand = !self.expand;
        heights
    }

    pub(crate) fn push(&mut self, item: Item) {
        match item {
            Item::User(text) => {
                self.push_block(Block::new(Kind::User, text));
            }
            Item::Help(skills) => {
                self.push_block(Block::new(Kind::Help, skills));
            }
            Item::Boxes { max, sections, note } => {
                self.push_block(Block::boxes(max, sections, note));
            }
            Item::Notice(color, text) => {
                self.push_block(Block::new(Kind::Notice(color), text));
            }
            Item::Aside(color, text) => {
                let b = Block::new(Kind::Notice(color), text);
                if self.blocks.last().is_some_and(Block::is_streaming_text) {
                    self.held.push(b);
                } else {
                    self.push_block(b);
                }
            }
        }
    }

    /// Last assistant reply (`tool_error == false`) or last failed tool output
    /// (`tool_error == true`), for `/copy`.
    pub(crate) fn copy_text(&self, tool_error: bool) -> Option<&str> {
        if tool_error {
            self.blocks.iter().rev().find(|b| b.kind == Kind::Tool && b.status == Status::Err).map(|b| b.text.as_str())
        } else {
            self.blocks.iter().rev().find(|b| b.kind == Kind::Assistant).map(|b| b.text.as_str())
        }
    }

    fn push_block(&mut self, b: Block) -> usize {
        self.blocks.append(&mut self.held);
        self.blocks.push(b);
        self.blocks.len() - 1
    }

    fn close_reasoning(&mut self) {
        if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.is_running_reasoning()) {
            b.close_reasoning();
        }
    }
}
