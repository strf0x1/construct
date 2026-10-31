//! The TUI's state: transcript blocks, the input line, and how agent events
//! change them.

use super::args::primary_arg;
use super::ask::Prompt;
use super::palette::{self, Palette, Query};
use super::picker::Picker;
use super::theme;
use crate::agent::Event;
use crate::config::{Config, Paths};
use crate::skills::{Library, Skill};
use crate::tools::{Catalog, OutputKind};
use ratatui::style::Color;
use ratatui::text::Line;
use std::collections::{HashMap, VecDeque};
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
    /// The `/help` panel; drawn by `help.rs`, so the block carries no text.
    Help,
    /// Rounded boxes (`boxed.rs`) up to this wide, from `Block::boxes`, with
    /// `Block::text` as a dim note underneath.
    Boxes(usize),
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
    /// For `Kind::Boxes`: what to draw.
    pub(super) boxes: Vec<super::boxed::Section>,
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
            boxes: Vec::new(),
        }
    }

    /// Boxes at most `max` wide, then `note` in dim text.
    pub(super) fn boxes(max: usize, boxes: Vec<super::boxed::Section>, note: impl Into<String>) -> Self {
        Self { boxes, ..Self::new(Kind::Boxes(max), note) }
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
    /// `/<skill> text`: run a turn with the skill loaded.
    Skill {
        name: String,
        text: String,
    },
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
    /// The session's accounting, for the status line: summaries and
    /// sub-agents included, the same sums `/status` reads from the journal.
    pub(super) totals: crate::journal::Totals,
    pub(super) context_pct: u64,
    pub(super) groups: Vec<String>,
    pub(super) learning: Option<(String, Instant)>,
    pub(super) dock: super::children::Dock,
    pub(super) toast: Option<(String, Color, Instant)>,
    pub(super) ready: bool,
    pub(super) session: String,
    pub(super) canceller: Option<watch::Sender<bool>>,
    pub(super) quit_armed: Option<Instant>,
    pub(super) stream_chars: usize,
    /// The session's tools, once the agent is built; display hints come from here.
    pub(super) catalog: Option<Arc<Catalog>>,
    /// Questions waiting for a key, oldest first; the front one is on screen.
    pub(super) prompts: VecDeque<Prompt>,
    /// The slash palette, while the input is a lone `/word`.
    pub(super) palette: Option<Palette>,
    /// The input esc closed the palette on; it stays closed until that changes.
    pub(super) palette_off: Option<String>,
    /// The model picker, while `/model` has it open.
    pub(super) picker: Option<Picker>,
    /// Project kit, then global library. Found on the first `/`, not at
    /// startup, so the first frame never waits on it.
    skill_libs: Option<Vec<Library>>,
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
            totals: Default::default(),
            context_pct: 0,
            groups: vec![],
            learning: None,
            dock: Default::default(),
            toast: None,
            ready: false,
            session: String::new(),
            canceller: None,
            quit_armed: None,
            stream_chars: 0,
            catalog: None,
            prompts: VecDeque::new(),
            palette: None,
            palette_off: None,
            picker: None,
            skill_libs: None,
        }
    }

    /// Every skill the user can run, read from disk now.
    pub(super) fn skills(&mut self) -> Vec<Skill> {
        let libs = self.skill_libs.get_or_insert_with(|| {
            let Ok(paths) = Paths::resolve() else { return vec![] };
            let cwd = std::env::current_dir().unwrap_or_default();
            let kit = crate::project::detect(&paths, &cwd).map(|p| Library::at(p.skills_dir()));
            kit.into_iter().chain([Library::new(&paths)]).collect()
        });
        crate::skills::merged(&libs.iter().collect::<Vec<_>>())
    }

    /// Open, narrow or close the palette to match the input. The skill
    /// library is re-read only when the query changes.
    pub(super) fn refresh_palette(&mut self) {
        let query = match palette::query(&self.input) {
            Some(q) if self.palette_off.as_deref() != Some(self.input.as_str()) => q,
            _ => {
                self.palette = None;
                return;
            }
        };
        let (q, models) = match query {
            Query::Command(q) => (q.to_string(), false),
            Query::Model(q) => (q.to_string(), true),
        };
        if self.palette.as_ref().is_some_and(|p| p.query == q && p.models == models) {
            return;
        }
        let items =
            if models { palette::model_candidates(&q, &self.cfg) } else { palette::candidates(&q, &self.skills()) };
        self.palette = (!items.is_empty()).then_some(Palette { query: q, models, items, selected: 0 });
    }

    /// How full the window is, as the status line says it.
    pub(super) fn context_pct_now(&self) -> u64 {
        self.totals.last_prompt * 100 / (self.cfg.context_window.max(1) as u64)
    }

    pub(super) fn animating(&self) -> bool {
        self.busy
            || !self.prompts.is_empty()
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
        // Anything billed counts, a summary's or a child's included.
        self.totals.add(&ev);
        match ev {
            // The record, not the screen: the UI already shows what these carry.
            Event::Manifest(_)
            | Event::Head { .. }
            | Event::Permission { .. }
            // Shown when the prompt is answered, so a child's are too.
            | Event::Access { .. }
            | Event::Ask { .. }
            | Event::System { .. }
            | Event::User { .. }
            // The user's own `/skill` line already says it.
            | Event::Invoke { .. }
            | Event::Assistant { .. }
            | Event::ToolResult { .. }
            | Event::Note { .. }
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
                self.end_call(&id);
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
            Event::Usage { .. } => self.context_pct = self.context_pct_now(),
            Event::Model { model, context_window, .. } => {
                let _ = self.cfg.select_model(&model); // the agent switched, so it resolves
                self.context_pct = self.context_pct_now();
                let label = self.cfg.entry().map(|e| e.label.as_str()).filter(|l| !l.is_empty());
                let what = label.map_or(model.clone(), |l| format!("{model} ({l})"));
                self.notice(theme::ACCENT, format!("model → {what} · {} ctx", crate::models::short_tokens(context_window as u64)));
            }
            Event::Compacted { before, after, mode, tokens_before, tokens_after, .. } => {
                let what = if tokens_before > 0 {
                    format!("{mode}, ~{}k → ~{}k tokens", tokens_before / 1000, tokens_after / 1000)
                } else {
                    format!("{before} → {after} messages")
                };
                self.notice(theme::ACCENT2, format!("◇ compacted history ({what})"));
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
                // The turn may have written a skill: list it if `/` is up.
                if self.palette.take().is_some() {
                    self.refresh_palette();
                }
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
            Event::Child { id, inner } => self.on_child(&id, *inner),
            Event::JobStart(_) | Event::JobEnd(_) => self.on_job(&ev),
            // The turn it starts or joins says it.
            Event::JobsDelivered(_) => {}
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

    /// Drop questions whose call has gone (interrupted, timed out).
    pub(super) fn prune_prompts(&mut self) {
        self.prompts.retain(|p| !p.stale());
    }

    /// Answer the question on screen with `answer`, and note it in the transcript.
    pub(super) fn answer_prompt(&mut self, answer: impl FnOnce(&mut Prompt) -> (String, bool)) {
        let Some(mut p) = self.prompts.pop_front() else { return };
        let (text, allowed) = answer(&mut p);
        self.notice(if allowed { theme::GREEN } else { theme::YELLOW }, format!("⚠ {text}"));
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
