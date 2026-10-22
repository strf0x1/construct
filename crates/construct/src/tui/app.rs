//! The TUI's state: transcript blocks, the input line, and how agent events
//! change them.

use super::theme;
use crate::agent::Event;
use crate::config::Config;
use crate::tools::{Catalog, OutputKind};
use ratatui::style::Color;
use ratatui::text::Line;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

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
}

pub(super) struct Block {
    pub(super) kind: Kind,
    pub(super) text: String,
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) status: Status,
    pub(super) started: Instant,
    pub(super) ms: u64,
    pub(super) id: String,
    pub(super) version: u64,
    /// For tool blocks: lines of output to preview, and how to render it.
    pub(super) preview: usize,
    pub(super) out: OutputKind,
}

impl Block {
    pub(super) fn new(kind: Kind, text: impl Into<String>) -> Self {
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
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Phase {
    Idle,
    Routing,
    Waiting,
    Thinking,
    Writing,
    Tool(String),
}

pub(super) enum Cmd {
    Prompt(String),
    New,
    Model(String),
    LearnNow,
}

pub(super) struct App {
    pub(super) cfg: Config,
    pub(super) blocks: Vec<Block>,
    pub(super) cache: HashMap<usize, (u16, u64, bool, Vec<Line<'static>>)>,
    pub(super) input: String,
    pub(super) cursor: usize,
    pub(super) history: Vec<String>,
    pub(super) hist_pos: Option<usize>,
    pub(super) scroll: usize,
    pub(super) last_total: usize,
    pub(super) phase: Phase,
    pub(super) busy: bool,
    pub(super) turn_started: Instant,
    pub(super) last_activity: Instant,
    pub(super) started: Instant,
    pub(super) expand: bool,
    pub(super) tokens: u64,
    pub(super) context_pct: u64,
    pub(super) groups: Vec<String>,
    pub(super) learning: Option<(String, Instant)>,
    pub(super) toast: Option<(String, Color, Instant)>,
    pub(super) ready: bool,
    pub(super) session: String,
    pub(super) canceller: Option<watch::Sender<bool>>,
    pub(super) quit_armed: Option<Instant>,
    pub(super) stream_chars: usize,
    /// The session's tools, once the agent is built; display hints come from here.
    pub(super) catalog: Option<Arc<Catalog>>,
}

/// The argument a tool's display hints name, or the raw arguments.
fn primary_arg(key: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let s = match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => args.to_string(),
    };
    let s = s.replace('\n', " ⏎ ");
    crate::llm::truncate(&s, 200)
}

impl App {
    pub(super) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            blocks: vec![],
            cache: HashMap::new(),
            input: String::new(),
            cursor: 0,
            history: vec![],
            hist_pos: None,
            scroll: 0,
            last_total: 0,
            phase: Phase::Idle,
            busy: false,
            turn_started: Instant::now(),
            last_activity: Instant::now(),
            started: Instant::now(),
            expand: false,
            tokens: 0,
            context_pct: 0,
            groups: vec![],
            learning: None,
            toast: None,
            ready: false,
            session: String::new(),
            canceller: None,
            quit_armed: None,
            stream_chars: 0,
            catalog: None,
        }
    }

    pub(super) fn animating(&self) -> bool {
        self.busy
            || self.learning.is_some()
            || self.started.elapsed() < Duration::from_millis(1600)
            || self.toast.as_ref().is_some_and(|t| t.2.elapsed() < Duration::from_secs(4))
    }

    pub(super) fn push(&mut self, b: Block) {
        self.blocks.push(b);
    }

    pub(super) fn last_of(&mut self, kind: Kind) -> Option<&mut Block> {
        match self.blocks.last_mut() {
            Some(b) if b.kind == kind => Some(b),
            _ => None,
        }
    }

    pub(super) fn notice(&mut self, color: Color, text: impl Into<String>) {
        self.push(Block::new(Kind::Notice(color), text));
    }

    pub(super) fn on_event(&mut self, ev: Event) {
        self.last_activity = Instant::now();
        match ev {
            // The record, not the screen: the UI already shows what these carry.
            Event::Manifest(_)
            | Event::Head { .. }
            | Event::System { .. }
            | Event::User { .. }
            | Event::Assistant { .. }
            | Event::ToolResult { .. }
            | Event::Request { .. } => {}
            Event::TurnStart => {
                self.busy = true;
                self.phase = Phase::Routing;
                self.turn_started = Instant::now();
                self.stream_chars = 0;
            }
            Event::Route { groups, skills, source, ms, note } => {
                self.phase = Phase::Waiting;
                if !skills.is_empty() || !groups.is_empty() {
                    let mut parts = vec![];
                    if !groups.is_empty() {
                        parts.push(format!("tools +{}", groups.join(" +")));
                    }
                    if !skills.is_empty() {
                        parts.push(format!("skills {}", skills.join(", ")));
                    }
                    self.notice(theme::FAINT, format!("↳ {} · {source} {ms}ms", parts.join(" · ")));
                }
                if !note.is_empty() && note.contains("failed") {
                    self.notice(theme::YELLOW, note);
                }
            }
            Event::Opened => {
                if self.phase == Phase::Waiting || self.phase == Phase::Routing {
                    self.phase = Phase::Thinking;
                }
            }
            Event::Reasoning { text } => {
                self.phase = Phase::Thinking;
                match self.last_of(Kind::Reasoning) {
                    Some(b) if b.status == Status::Running => {
                        b.text.push_str(&text);
                        b.version += 1;
                    }
                    _ => {
                        let mut b = Block::new(Kind::Reasoning, text);
                        b.status = Status::Running;
                        self.push(b);
                    }
                }
            }
            Event::Text { text } => {
                self.phase = Phase::Writing;
                self.stream_chars += text.len();
                self.close_reasoning();
                match self.last_of(Kind::Assistant) {
                    Some(b) => {
                        b.text.push_str(&text);
                        b.version += 1;
                    }
                    None => {
                        let mut b = Block::new(Kind::Assistant, text);
                        b.status = Status::Running;
                        self.push(b);
                    }
                }
            }
            Event::ToolPending { name } => {
                self.close_reasoning();
                self.phase = Phase::Tool(format!("preparing {name}"));
            }
            Event::ToolStart { id, name, args } => {
                self.close_reasoning();
                let mut b = Block::new(Kind::Tool, "");
                let d = self.catalog.as_ref().map(|c| c.display(&name)).unwrap_or_default();
                b.detail = primary_arg(d.primary_arg, &args);
                b.preview = d.preview_lines;
                b.title = name.clone();
                b.id = id;
                b.status = Status::Running;
                self.phase = Phase::Tool(name);
                self.push(b);
            }
            Event::ToolEnd { id, ok, output, ms, kind, .. } => {
                if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.kind == Kind::Tool && b.id == id) {
                    b.status = if ok { Status::Ok } else { Status::Err };
                    b.text = output;
                    b.ms = ms;
                    b.out = kind;
                    b.version += 1;
                }
                if !self.blocks.iter().any(|b| b.kind == Kind::Tool && b.status == Status::Running) {
                    self.phase = Phase::Waiting;
                }
            }
            Event::Usage { prompt, completion, .. } => {
                self.tokens += prompt + completion;
                self.context_pct = prompt * 100 / (self.cfg.context_window.max(1) as u64);
            }
            Event::Compacted { before, after } => {
                self.notice(theme::ACCENT2, format!("◇ compacted history: {before} → {after} messages"));
            }
            Event::TurnEnd { steps, ms, interrupted, .. } => {
                self.close_reasoning();
                for b in &mut self.blocks {
                    if b.status == Status::Running {
                        b.status = if b.kind == Kind::Tool { Status::Err } else { Status::Ok };
                        b.version += 1;
                    }
                }
                self.busy = false;
                self.phase = Phase::Idle;
                let secs = ms as f32 / 1000.0;
                if interrupted {
                    self.notice(theme::YELLOW, format!("■ interrupted after {secs:.1}s"));
                } else if steps > 1 {
                    self.notice(theme::FAINT, format!("· {steps} steps · {secs:.1}s"));
                }
            }
            Event::Error { message } => {
                self.close_reasoning();
                self.notice(theme::RED, format!("✗ {message}"));
            }
            Event::Learn { status } => {
                if status.starts_with("learning") {
                    self.learning = Some((status, Instant::now()));
                } else {
                    self.learning = None;
                    self.toast = Some((status, theme::ACCENT2, Instant::now()));
                }
            }
        }
    }

    pub(super) fn close_reasoning(&mut self) {
        if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.kind == Kind::Reasoning && b.status == Status::Running)
        {
            b.status = Status::Ok;
            b.ms = b.started.elapsed().as_millis() as u64;
            b.version += 1;
        }
    }
}
