//! Full-screen TUI. The first frame is drawn before the agent is built, every
//! frame is a synchronized update (no tearing), and frames are drawn only
//! when something changed: the animation tick runs only while work is
//! visibly in progress.

mod markdown;
mod theme;

use crate::agent::{Agent, Event};
use crate::config::{Config, Paths};
use crate::learn;
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as CEvent, EventStream,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::HashMap;
use std::io::{Stdout, Write};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Status {
    Running,
    Ok,
    Err,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Notice(Color),
}

struct Block {
    kind: Kind,
    text: String,
    title: String,
    detail: String,
    status: Status,
    started: Instant,
    ms: u64,
    id: String,
    version: u64,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    Routing,
    Waiting,
    Thinking,
    Writing,
    Tool(String),
}

enum Cmd {
    Prompt(String),
    New,
    Model(String),
    LearnNow,
}

struct App {
    cfg: Config,
    blocks: Vec<Block>,
    cache: HashMap<usize, (u16, u64, bool, Vec<Line<'static>>)>,
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    scroll: usize,
    last_total: usize,
    phase: Phase,
    busy: bool,
    turn_started: Instant,
    last_activity: Instant,
    started: Instant,
    expand: bool,
    tokens: u64,
    context_pct: u64,
    groups: Vec<String>,
    learning: Option<(String, Instant)>,
    toast: Option<(String, Color, Instant)>,
    ready: bool,
    session: String,
    canceller: Option<watch::Sender<bool>>,
    quit_armed: Option<Instant>,
    stream_chars: usize,
}

fn primary_arg(name: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let key = match name {
        "shell" => "command",
        "read" | "write" | "edit" => "path",
        "grep" | "glob" => "pattern",
        "web_fetch" => "url",
        "web_search" | "session_search" => "query",
        "skill" => "name",
        "skill_manage" => "action",
        "memory" => "target",
        "enable_tools" => "groups",
        _ => "",
    };
    let s = match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => args.to_string(),
    };
    let s = s.replace('\n', " ⏎ ");
    crate::llm::truncate(&s, 200)
}

impl App {
    fn new(cfg: Config) -> Self {
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
        }
    }

    fn animating(&self) -> bool {
        self.busy
            || self.learning.is_some()
            || self.started.elapsed() < Duration::from_millis(1600)
            || self.toast.as_ref().is_some_and(|t| t.2.elapsed() < Duration::from_secs(4))
    }

    fn push(&mut self, b: Block) {
        self.blocks.push(b);
    }

    fn last_of(&mut self, kind: Kind) -> Option<&mut Block> {
        match self.blocks.last_mut() {
            Some(b) if b.kind == kind => Some(b),
            _ => None,
        }
    }

    fn notice(&mut self, color: Color, text: impl Into<String>) {
        self.push(Block::new(Kind::Notice(color), text));
    }

    fn on_event(&mut self, ev: Event) {
        self.last_activity = Instant::now();
        match ev {
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
                b.detail = primary_arg(&name, &args);
                b.title = name.clone();
                b.id = id;
                b.status = Status::Running;
                self.phase = Phase::Tool(name);
                self.push(b);
            }
            Event::ToolEnd { id, ok, output, ms, .. } => {
                if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.kind == Kind::Tool && b.id == id) {
                    b.status = if ok { Status::Ok } else { Status::Err };
                    b.text = output;
                    b.ms = ms;
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

    fn close_reasoning(&mut self) {
        if let Some(b) = self.blocks.iter_mut().rev().find(|b| b.kind == Kind::Reasoning && b.status == Status::Running) {
            b.status = Status::Ok;
            b.ms = b.started.elapsed().as_millis() as u64;
            b.version += 1;
        }
    }

    // ---------- rendering ----------

    fn block_lines(&mut self, i: usize, width: u16, phase: f32) -> Vec<Line<'static>> {
        let expand = self.expand;
        let b = &self.blocks[i];
        let dynamic = b.status == Status::Running;
        if !dynamic {
            if let Some((w, v, e, lines)) = self.cache.get(&i) {
                if *w == width && *v == b.version && *e == expand {
                    return lines.clone();
                }
            }
        }
        let w = width as usize;
        let mut out: Vec<Line<'static>> = Vec::new();
        match &b.kind {
            Kind::User => {
                let bar = Span::styled("▌ ", Style::new().fg(theme::ACCENT2));
                for (n, para) in b.text.split('\n').enumerate() {
                    let style = Style::new().add_modifier(Modifier::BOLD);
                    let lines = markdown::wrap(vec![Span::styled(para.to_string(), style)], w, vec![bar.clone()], vec![bar.clone()]);
                    let _ = n;
                    out.extend(lines);
                }
            }
            Kind::Assistant => {
                let md = markdown::render(&b.text, w.saturating_sub(2));
                for l in md {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(l.spans);
                    out.push(Line::from(spans));
                }
            }
            Kind::Reasoning => {
                let st = Style::new().fg(theme::DIM).add_modifier(Modifier::ITALIC);
                if b.status == Status::Running || expand {
                    let label = if b.status == Status::Running { "thinking" } else { "thought" };
                    let mut head = vec![Span::styled("  ✻ ", Style::new().fg(theme::ACCENT2))];
                    head.push(Span::styled(label.to_string(), theme::dim()));
                    out.push(Line::from(head));
                    let text = b.text.trim();
                    let lines: Vec<Line<'static>> = text
                        .split('\n')
                        .flat_map(|p| markdown::wrap(vec![Span::styled(p.to_string(), st)], w, vec![Span::raw("    ")], vec![Span::raw("    ")]))
                        .collect();
                    let keep = if expand { 400 } else { 3 };
                    let skip = lines.len().saturating_sub(keep);
                    out.extend(lines.into_iter().skip(skip));
                } else {
                    out.push(Line::from(vec![
                        Span::styled("  ✻ ", Style::new().fg(theme::FAINT)),
                        Span::styled(format!("thought for {:.1}s", b.ms as f32 / 1000.0), theme::faint()),
                    ]));
                }
            }
            Kind::Tool => {
                let (icon, color) = match b.status {
                    Status::Running => (theme::spinner(phase).to_string(), theme::YELLOW),
                    Status::Ok => ("✓".into(), theme::GREEN),
                    Status::Err => ("✗".into(), theme::RED),
                };
                let mut head = vec![
                    Span::styled(format!("  {icon} "), Style::new().fg(color)),
                    Span::styled(b.title.clone(), Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
                    Span::raw(" "),
                ];
                let time = match b.status {
                    Status::Running => format!(" {:.1}s", b.started.elapsed().as_secs_f32()),
                    _ if b.ms >= 1000 => format!(" {:.1}s", b.ms as f32 / 1000.0),
                    _ => format!(" {}ms", b.ms),
                };
                let room = w.saturating_sub(5 + b.title.width() + time.width());
                let detail = fit(&b.detail, room);
                head.push(Span::styled(detail, theme::dim()));
                head.push(Span::styled(time, theme::faint()));
                out.push(Line::from(head));
                if b.status != Status::Running && !b.text.is_empty() {
                    let max = if expand { 60 } else if b.status == Status::Err { 4 } else { tool_preview_lines(&b.title) };
                    let body = tool_body(&b.title, &b.text);
                    let lines: Vec<&str> = body.lines().collect();
                    for l in lines.iter().take(max) {
                        let style = if b.title == "edit" || b.title == "write" {
                            match l.chars().next() {
                                Some('+') => Style::new().fg(theme::GREEN),
                                Some('-') => Style::new().fg(theme::RED),
                                _ => theme::faint(),
                            }
                        } else if b.status == Status::Err {
                            Style::new().fg(theme::RED)
                        } else {
                            theme::faint()
                        };
                        out.push(Line::from(vec![Span::styled("    │ ", theme::faint()), Span::styled(fit(&l.replace('\t', "  "), w.saturating_sub(6)), style)]));
                    }
                    if max == 0 && !expand {
                        out.push(Line::from(Span::styled(format!("    │ {} lines", lines.len()), theme::faint())));
                    } else if lines.len() > max {
                        out.push(Line::from(Span::styled(
                            format!("    │ … {} more lines{}", lines.len() - max, if expand { "" } else { " (ctrl+o)" }),
                            theme::faint(),
                        )));
                    }
                }
            }
            Kind::Notice(color) => {
                let st = Style::new().fg(*color);
                out.extend(markdown::wrap(vec![Span::styled(b.text.clone(), st)], w, vec![Span::raw("  ")], vec![Span::raw("    ")]));
            }
        }
        // spacing: a blank line before user turns and assistant text
        let gap = matches!(b.kind, Kind::User | Kind::Assistant)
            || (b.kind == Kind::Tool && i > 0 && !matches!(self.blocks[i - 1].kind, Kind::Tool));
        if gap && i > 0 {
            out.insert(0, Line::default());
        }
        if !dynamic {
            self.cache.insert(i, (width, b.version, expand, out.clone()));
        }
        out
    }

    fn transcript(&mut self, width: u16, phase: f32) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for i in 0..self.blocks.len() {
            lines.extend(self.block_lines(i, width, phase));
        }
        lines
    }

    fn splash(&self, area: Rect, phase: f32) -> Vec<Line<'static>> {
        let mut lines = vec![];
        let top = area.height.saturating_sub(9) / 3;
        for _ in 0..top {
            lines.push(Line::default());
        }
        let title = "◆ construct";
        let mut t = vec![Span::raw("  ")];
        if phase < 1.6 {
            t.extend(theme::shimmer(title, phase, theme::ACCENT, Color::Rgb(235, 235, 255)));
        } else {
            t.extend(title.chars().enumerate().map(|(i, c)| {
                Span::styled(c.to_string(), Style::new().fg(theme::lerp(theme::ACCENT, theme::ACCENT2, i as f32 / 10.0)).add_modifier(Modifier::BOLD))
            }));
        }
        lines.push(Line::from(t));
        lines.push(Line::from(Span::styled("  lean · fast · learns as it goes", theme::dim())));
        lines.push(Line::default());
        let row = |k: &str, v: String| Line::from(vec![Span::styled(format!("  {k:<9}"), theme::faint()), Span::styled(v, theme::dim())]);
        lines.push(row("model", self.cfg.model.clone()));
        lines.push(row("endpoint", self.cfg.base_url.clone()));
        lines.push(row("router", self.cfg.router.mode.clone()));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  enter send · alt+enter newline · esc interrupt · ctrl+o expand · pgup/pgdn scroll · /help",
            theme::faint(),
        )));
        lines
    }

    fn input_lines(&self, width: usize) -> (Vec<Line<'static>>, (u16, u16)) {
        let prompt_w = 2;
        let avail = width.saturating_sub(prompt_w + 1).max(4);
        let mut lines = vec![];
        let mut cur = (0u16, prompt_w as u16);
        let mut row = 0u16;
        let mut offset = 0usize;
        let placeholder = self.input.is_empty();
        let text = if placeholder {
            if self.busy { "queue a message… (esc to interrupt)" } else { "ask construct anything… (/help)" }.to_string()
        } else {
            self.input.clone()
        };
        for (pi, para) in text.split('\n').enumerate() {
            let mut chunk = String::new();
            let mut cw = 0;
            let mut chunks: Vec<(String, usize)> = vec![];
            let mut start = offset;
            for (bi, c) in para.char_indices() {
                let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                if cw + w > avail {
                    chunks.push((std::mem::take(&mut chunk), start));
                    start = offset + bi;
                    cw = 0;
                }
                chunk.push(c);
                cw += w;
            }
            chunks.push((chunk, start));
            for (ci, (c, s)) in chunks.iter().enumerate() {
                let prefix = if pi == 0 && ci == 0 { Span::styled("› ", Style::new().fg(theme::ACCENT2).add_modifier(Modifier::BOLD)) } else { Span::raw("  ") };
                let style = if placeholder { theme::faint() } else { Style::default() };
                lines.push(Line::from(vec![prefix, Span::styled(c.clone(), style)]));
                if !placeholder && self.cursor >= *s && self.cursor <= s + c.len() {
                    let before = &self.input[*s..self.cursor];
                    cur = (row, (prompt_w + before.width()) as u16);
                }
                row += 1;
            }
            offset += para.len() + 1;
        }
        (lines, cur)
    }

    fn activity(&self, phase: f32) -> Line<'static> {
        if !self.busy {
            return Line::default();
        }
        let since = self.last_activity.elapsed().as_secs_f32();
        let total = self.turn_started.elapsed().as_secs_f32();
        let (mut label, mut base, mut hi) = match &self.phase {
            Phase::Routing => ("routing".to_string(), theme::DIM, theme::CYAN),
            Phase::Waiting => ("waiting for model".to_string(), theme::DIM, theme::ACCENT),
            Phase::Thinking => ("thinking".to_string(), theme::DIM, theme::ACCENT2),
            Phase::Writing => ("writing".to_string(), theme::DIM, theme::ACCENT),
            Phase::Tool(n) => (format!("running {n}"), theme::DIM, theme::YELLOW),
            Phase::Idle => (String::new(), theme::DIM, theme::DIM),
        };
        // Silence from the model reads as "broken"; say so plainly instead.
        if since > 6.0 && !matches!(self.phase, Phase::Tool(_)) {
            label = format!("{label} · no output for {since:.0}s");
            base = theme::YELLOW;
            hi = Color::Rgb(255, 230, 180);
        }
        let mut spans = vec![Span::styled(format!("  {} ", theme::spinner(phase)), Style::new().fg(hi))];
        spans.extend(theme::shimmer(&label, phase, base, hi));
        spans.push(Span::styled(format!("  {total:.1}s"), theme::faint()));
        if self.phase == Phase::Writing && total > 0.5 {
            spans.push(Span::styled(format!(" · {:.0} chars/s", self.stream_chars as f32 / total), theme::faint()));
        }
        spans.push(Span::styled("  esc to interrupt", theme::faint()));
        Line::from(spans)
    }

    fn status(&self, width: usize, phase: f32) -> Line<'static> {
        let left = vec![
            Span::styled(" ◆ ", Style::new().fg(theme::ACCENT2)),
            Span::styled(self.cfg.model.clone(), theme::dim()),
        ];
        let mut right: Vec<Span<'static>> = vec![];
        if let Some((s, _)) = &self.learning {
            right.push(Span::styled(format!("◇ {s}  "), Style::new().fg(theme::pulse(phase, theme::FAINT, theme::ACCENT2))));
        } else if let Some((t, c, at)) = &self.toast {
            if at.elapsed() < Duration::from_secs(6) {
                right.push(Span::styled(format!("◇ {}  ", crate::llm::truncate(t, 60)), Style::new().fg(*c)));
            }
        }
        if !self.groups.is_empty() {
            right.push(Span::styled(format!("{}  ", self.groups.join(" ")), theme::faint()));
        }
        right.push(Span::styled(format!("{} tok", human(self.tokens)), theme::faint()));
        if self.context_pct > 0 {
            right.push(Span::styled(format!(" · ctx {}%", self.context_pct), if self.context_pct > 70 { Style::new().fg(theme::YELLOW) } else { theme::faint() }));
        }
        if self.scroll > 0 {
            right.push(Span::styled(format!(" · ↑{}", self.scroll), Style::new().fg(theme::CYAN)));
        }
        right.push(Span::raw(" "));
        let lw: usize = left.iter().map(|s| s.content.width()).sum();
        let rw: usize = right.iter().map(|s| s.content.width()).sum();
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(width.saturating_sub(lw + rw))));
        spans.extend(right);
        Line::from(spans)
    }

    fn draw(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let phase = self.started.elapsed().as_secs_f32();
        let size = term.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let width = area.width;
        let (input, cursor) = self.input_lines(width as usize);
        let input_h = (input.len() as u16).clamp(1, 8);
        let bottom = input_h + 3; // activity + rule + input + status
        let main_h = area.height.saturating_sub(bottom);
        let body = if self.blocks.is_empty() { self.splash(area, phase) } else { self.transcript(width.saturating_sub(1), phase) };
        let total = body.len();
        if self.scroll > 0 && total > self.last_total {
            self.scroll += total - self.last_total;
        }
        self.last_total = total;
        let max_scroll = total.saturating_sub(main_h as usize);
        self.scroll = self.scroll.min(max_scroll);
        let end = total - self.scroll;
        let start = end.saturating_sub(main_h as usize);
        let visible: Vec<Line<'static>> = body[start..end].to_vec();
        let activity = self.activity(phase);
        let status = self.status(width as usize, phase);
        let input_scroll = (input.len() as u16).saturating_sub(input_h);
        let rule_color = if self.busy { theme::pulse(phase, theme::FAINT, theme::ACCENT) } else { theme::FAINT };
        let mut stdout = std::io::stdout();
        queue!(stdout, BeginSynchronizedUpdate)?;
        term.draw(|f| {
            // pad from the top so short transcripts sit right above the input
            let pad = main_h.saturating_sub(visible.len() as u16);
            f.render_widget(Paragraph::new(visible.clone()), Rect::new(0, pad, width, main_h - pad));
            f.render_widget(Paragraph::new(activity.clone()), Rect::new(0, main_h, width, 1));
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("─".repeat(width as usize), Style::new().fg(rule_color)))),
                Rect::new(0, main_h + 1, width, 1),
            );
            f.render_widget(Paragraph::new(input.clone()).scroll((input_scroll, 0)), Rect::new(0, main_h + 2, width, input_h));
            f.render_widget(Paragraph::new(status.clone()), Rect::new(0, main_h + 2 + input_h, width, 1));
            let cy = cursor.0.saturating_sub(input_scroll).min(input_h - 1);
            f.set_cursor_position(Position::new(cursor.1.min(width - 1), main_h + 2 + cy));
        })?;
        queue!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;
        Ok(())
    }

    // ---------- input ----------

    fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn prev_boundary(&self) -> usize {
        self.input[..self.cursor].char_indices().last().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.input[self.cursor..].char_indices().nth(1).map(|(i, _)| self.cursor + i).unwrap_or(self.input.len())
    }
}

fn tool_preview_lines(name: &str) -> usize {
    match name {
        "edit" | "write" => 8,
        "shell" => 4,
        "read" | "skill" => 0,
        _ => 2,
    }
}

/// What to show under a finished tool: diffs for edits, the tail of shell.
fn tool_body(name: &str, out: &str) -> String {
    match name {
        "edit" => out.lines().skip(1).collect::<Vec<_>>().join("\n"),
        "shell" => {
            let lines: Vec<&str> = out.lines().collect();
            if lines.len() > 4 { lines[lines.len() - 4..].join("\n") + &format!("\n({} lines)", lines.len()) } else { out.to_string() }
        }
        _ => out.to_string(),
    }
}

fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cw = 0;
    for c in s.chars() {
        let x = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cw + x + 1 > w {
            break;
        }
        out.push(c);
        cw += x;
    }
    out.push('…');
    out
}

fn human(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}

struct Guard {
    kbd: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore(self.kbd);
    }
}

fn restore(kbd: bool) {
    let mut out = std::io::stdout();
    if kbd {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = terminal::disable_raw_mode();
}

/// Point fd 2 at a log file while the TUI owns the terminal, so a library
/// writing to stderr can't scribble over the screen. Returns the saved fd.
fn quiet_stderr(paths: &Paths) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let _ = std::fs::create_dir_all(&paths.home);
    let f = std::fs::OpenOptions::new().create(true).append(true).open(paths.home.join("stderr.log")).ok()?;
    // SAFETY: dup/dup2 on valid descriptors; the file outlives the call via dup2.
    unsafe {
        let saved = libc::dup(2);
        if saved < 0 || libc::dup2(f.as_raw_fd(), 2) < 0 {
            return None;
        }
        Some(saved)
    }
}

fn restore_stderr(saved: Option<i32>) {
    if let Some(fd) = saved {
        // SAFETY: restoring the descriptor saved by quiet_stderr.
        unsafe {
            libc::dup2(fd, 2);
            libc::close(fd);
        }
    }
}

pub async fn run(cfg: Config, paths: Paths, resume: Option<String>) -> Result<()> {
    let saved_stderr = quiet_stderr(&paths);
    let r = run_inner(cfg, paths, resume).await;
    restore_stderr(saved_stderr);
    r
}

async fn run_inner(cfg: Config, paths: Paths, resume: Option<String>) -> Result<()> {
    trace("tui start");
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    // Push unconditionally: asking the terminal first costs up to 2s on
    // terminals that never answer, and the flags are ignored where unsupported.
    let kbd = true;
    let _ = execute!(stdout, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    let _guard = Guard { kbd };
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(kbd);
        hook(info);
    }));
    // No term.clear(): it asks the terminal for the cursor position (a round
    // trip), and a freshly entered alternate screen is already blank.
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut app = App::new(cfg.clone());
    app.draw(&mut term)?;
    trace("first frame");

    // Build the agent off the UI path; the first frame is already up.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<(String, watch::Sender<bool>, Vec<String>, Vec<crate::llm::Message>)>();
    let worker = tokio::spawn(agent_task(cfg.clone(), paths.clone(), resume, cmd_rx, ev_tx, ready_tx));

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = false;
    let mut last_draw = Instant::now();
    loop {
        let animating = app.animating();
        tokio::select! {
            Some(ready) = ready_rx.recv() => {
                let (session, canceller, groups, history) = ready;
                app.ready = true;
                app.session = session;
                app.canceller = Some(canceller);
                app.groups = groups;
                for m in history {
                    match m.role.as_str() {
                        "user" => app.push(Block::new(Kind::User, m.content.unwrap_or_default().split("\n\n<system-reminder>").next().unwrap_or("").to_string())),
                        "assistant" if m.content.is_some() => app.push(Block::new(Kind::Assistant, m.content.unwrap_or_default())),
                        _ => {}
                    }
                }
                dirty = true;
            }
            Some(ev) = ev_rx.recv() => {
                if let Event::Error { message } = &ev {
                    if message.starts_with("fatal:") {
                        app.notice(theme::RED, message.clone());
                    }
                }
                if let Event::Route { groups, .. } = &ev {
                    for g in groups { if !app.groups.contains(g) { app.groups.push(g.clone()); } }
                }
                app.on_event(ev);
                // batch whatever else is already queued before drawing
                while let Ok(ev) = ev_rx.try_recv() { app.on_event(ev); }
                dirty = true;
            }
            Some(Ok(ev)) = events.next() => {
                match ev {
                    CEvent::Key(k) if k.kind != KeyEventKind::Release => {
                        if handle_key(&mut app, k, &cmd_tx) { break; }
                    }
                    CEvent::Paste(s) => { let s = s.replace("\r\n", "\n").replace('\r', "\n"); app.insert(&s); }
                    CEvent::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => app.scroll += 3,
                        MouseEventKind::ScrollDown => app.scroll = app.scroll.saturating_sub(3),
                        _ => continue,
                    },
                    CEvent::Resize(..) => { app.cache.clear(); }
                    _ => continue,
                }
                dirty = true;
            }
            _ = tick.tick(), if animating => { dirty = true; }
            // a frame held back by the redraw cap still gets drawn
            _ = tokio::time::sleep_until((last_draw + Duration::from_millis(8)).into()), if dirty => {}
        }
        // Cap redraws at ~120fps; a burst of deltas becomes one frame.
        if dirty && last_draw.elapsed() >= Duration::from_millis(8) {
            app.draw(&mut term)?;
            last_draw = Instant::now();
            dirty = false;
        }
    }
    if let Some(c) = &app.canceller {
        c.send_replace(true);
    }
    drop(cmd_tx);
    drop(_guard);
    let _ = tokio::time::timeout(Duration::from_millis(1500), worker).await;
    Ok(())
}

/// Returns true to quit.
fn handle_key(app: &mut App, k: KeyEvent, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('c') if ctrl => {
            if !app.input.is_empty() {
                app.input.clear();
                app.cursor = 0;
            } else if app.busy {
                if let Some(c) = &app.canceller {
                    c.send_replace(true);
                }
            } else if app.quit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                return true;
            } else {
                app.quit_armed = Some(Instant::now());
                app.toast = Some(("press ctrl+c again to quit".into(), theme::YELLOW, Instant::now()));
            }
        }
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return true,
        KeyCode::Esc => {
            if app.busy {
                if let Some(c) = &app.canceller {
                    c.send_replace(true);
                }
            } else {
                app.scroll = 0;
            }
        }
        KeyCode::Char('o') if ctrl => {
            app.expand = !app.expand;
        }
        KeyCode::Char('l') if ctrl => app.cache.clear(),
        KeyCode::Enter if alt || shift => app.insert("\n"),
        KeyCode::Char('j') if ctrl => app.insert("\n"),
        KeyCode::Enter => {
            // A trailing backslash continues the line, like a shell.
            if app.input.ends_with('\\') && app.cursor == app.input.len() {
                app.input.pop();
                app.cursor -= 1;
                app.insert("\n");
                return false;
            }
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return false;
            }
            app.history.push(text.clone());
            app.hist_pos = None;
            app.input.clear();
            app.cursor = 0;
            app.scroll = 0;
            if let Some(rest) = text.strip_prefix('/') {
                return slash(app, rest, cmd);
            }
            app.push(Block::new(Kind::User, text.clone()));
            let _ = cmd.send(Cmd::Prompt(text));
        }
        KeyCode::Backspace if ctrl || alt => delete_word(app),
        KeyCode::Char('w') if ctrl => delete_word(app),
        KeyCode::Backspace => {
            if app.cursor > 0 {
                let p = app.prev_boundary();
                app.input.replace_range(p..app.cursor, "");
                app.cursor = p;
            }
        }
        KeyCode::Delete => {
            if app.cursor < app.input.len() {
                let n = app.next_boundary();
                app.input.replace_range(app.cursor..n, "");
            }
        }
        KeyCode::Left => app.cursor = app.prev_boundary(),
        KeyCode::Right => app.cursor = app.next_boundary(),
        KeyCode::Home => app.cursor = app.input[..app.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0),
        KeyCode::End => app.cursor = app.input[app.cursor..].find('\n').map(|i| app.cursor + i).unwrap_or(app.input.len()),
        KeyCode::Char('a') if ctrl => app.cursor = 0,
        KeyCode::Char('e') if ctrl => app.cursor = app.input.len(),
        KeyCode::Char('u') if ctrl => {
            app.input.replace_range(..app.cursor, "");
            app.cursor = 0;
        }
        KeyCode::Char('k') if ctrl => app.input.truncate(app.cursor),
        KeyCode::Up if !app.input.contains('\n') => {
            if app.history.is_empty() {
                return false;
            }
            let i = match app.hist_pos {
                Some(0) => 0,
                Some(i) => i - 1,
                None => app.history.len() - 1,
            };
            app.hist_pos = Some(i);
            app.input = app.history[i].clone();
            app.cursor = app.input.len();
        }
        KeyCode::Down if !app.input.contains('\n') => {
            if let Some(i) = app.hist_pos {
                if i + 1 < app.history.len() {
                    app.hist_pos = Some(i + 1);
                    app.input = app.history[i + 1].clone();
                } else {
                    app.hist_pos = None;
                    app.input.clear();
                }
                app.cursor = app.input.len();
            }
        }
        KeyCode::PageUp => app.scroll += 10,
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Char(c) if !ctrl => {
            let mut b = [0u8; 4];
            app.insert(c.encode_utf8(&mut b));
        }
        KeyCode::Tab => app.insert("    "),
        _ => {}
    }
    false
}

/// CONSTRUCT_TRACE=1 logs startup milestones to $CONSTRUCT_HOME/trace.log.
pub fn trace(what: &str) {
    if std::env::var_os("CONSTRUCT_TRACE").is_none() {
        return;
    }
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed();
    if let Ok(p) = Paths::resolve() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p.home.join("trace.log")) {
            let _ = writeln!(f, "{:>8.2}ms {what}", t.as_secs_f64() * 1000.0);
        }
    }
}

fn delete_word(app: &mut App) {
    let before = &app.input[..app.cursor];
    let trimmed = before.trim_end();
    let start = trimmed.rfind(|c: char| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
    app.input.replace_range(start..app.cursor, "");
    app.cursor = start;
}

fn slash(app: &mut App, rest: &str, cmd: &mpsc::UnboundedSender<Cmd>) -> bool {
    let (name, arg) = rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
    match name {
        "quit" | "exit" | "q" => return true,
        "new" | "clear" => {
            app.blocks.clear();
            app.cache.clear();
            app.tokens = 0;
            app.context_pct = 0;
            app.groups.clear();
            let _ = cmd.send(Cmd::New);
        }
        "model" if !arg.is_empty() => {
            app.cfg.model = arg.to_string();
            let _ = cmd.send(Cmd::Model(arg.to_string()));
            app.notice(theme::ACCENT, format!("model → {arg}"));
        }
        "learn" => {
            let _ = cmd.send(Cmd::LearnNow);
        }
        "memory" => {
            let paths = Paths::resolve().ok();
            let block = paths.map(|p| crate::memory::Store::new(&p, &app.cfg.memory).prompt_block()).unwrap_or_default();
            app.notice(theme::DIM, if block.is_empty() { "memory is empty".to_string() } else { block });
        }
        "skills" => {
            let list = Paths::resolve().map(|p| crate::skills::Library::new(&p).list()).unwrap_or_default();
            let text = if list.is_empty() {
                "no skills yet; the learning pass writes them as you work".to_string()
            } else {
                list.iter().map(|s| format!("• {} — {}", s.name, s.description)).collect::<Vec<_>>().join("\n")
            };
            app.notice(theme::DIM, text);
        }
        "help" | "?" => app.notice(
            theme::DIM,
            "/new  fresh session · /model <id>  switch model · /skills · /memory · /learn  run the learning pass now · /quit\nenter send · alt+enter or ctrl+j newline · esc interrupt · ctrl+o expand tool output · pgup/pgdn or wheel scroll · ctrl+c clear/quit",
        ),
        other => app.notice(theme::YELLOW, format!("unknown command /{other} — try /help")),
    }
    false
}

async fn agent_task(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    tx: mpsc::UnboundedSender<Event>,
    ready: mpsc::UnboundedSender<(String, watch::Sender<bool>, Vec<String>, Vec<crate::llm::Message>)>,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let build = |resume: Option<String>, cfg: Config| Agent::new(cfg, paths.clone(), cwd.clone(), resume);
    let mut agent = match build(resume, cfg.clone()) {
        Ok(a) => a,
        Err(e) => {
            let _ = tx.send(Event::Error { message: format!("fatal: {e:#}") });
            return;
        }
    };
    trace("agent ready");
    agent.client.warm();
    let history: Vec<crate::llm::Message> = agent.messages.iter().skip(1).cloned().collect();
    let _ = ready.send((agent.log.id.clone(), agent.canceller(), vec![], history));
    let mut reviewed = agent.messages.len();
    while let Some(c) = cmds.recv().await {
        match c {
            Cmd::Prompt(text) => {
                let _ = agent.turn(&text, &tx).await;
                maybe_learn(&mut agent, &tx, &mut reviewed, false);
            }
            Cmd::LearnNow => maybe_learn(&mut agent, &tx, &mut reviewed, true),
            Cmd::Model(m) => agent.cfg.model = m,
            Cmd::New => {
                finish(&agent, reviewed);
                let mut c = agent.cfg.clone();
                c.model = agent.cfg.model.clone();
                match build(None, c) {
                    Ok(a) => {
                        agent = a;
                        reviewed = agent.messages.len();
                        let _ = ready.send((agent.log.id.clone(), agent.canceller(), vec![], vec![]));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error { message: format!("{e:#}") });
                    }
                }
            }
        }
    }
    finish(&agent, reviewed);
}

/// Hand any unreviewed tail of the session to a detached learning process.
fn finish(agent: &Agent, reviewed: usize) {
    if !agent.cfg.learn.enabled || reviewed >= agent.messages.len() {
        return;
    }
    let tail = &agent.messages[reviewed..];
    let users = tail.iter().filter(|m| m.role == "user").count() as u32;
    let calls = tail.iter().map(|m| m.tool_calls.len() as u32).sum();
    if learn::kind_for(users, calls).is_some() {
        let mut st = learn::state(&agent.paths);
        st["reviewed"][&agent.log.id] = serde_json::json!(reviewed);
        learn::save_state(&agent.paths, &st);
        learn::spawn_detached(&agent.log.id);
    }
}

fn maybe_learn(agent: &mut Agent, tx: &mpsc::UnboundedSender<Event>, reviewed: &mut usize, force: bool) {
    if !agent.cfg.learn.enabled && !force {
        return;
    }
    let mem = agent.turns_since_memory >= agent.cfg.learn.memory_every_turns;
    let skill = agent.tool_calls_since_skill >= agent.cfg.learn.skill_every_tool_calls;
    let kind = match (mem || force, skill || force) {
        (true, true) => learn::Kind::Both,
        (true, false) => learn::Kind::Memory,
        (false, true) => learn::Kind::Skills,
        _ => return,
    };
    if *reviewed >= agent.messages.len() {
        return;
    }
    agent.turns_since_memory = 0;
    agent.tool_calls_since_skill = 0;
    let slice = agent.messages[*reviewed..].to_vec();
    *reviewed = agent.messages.len();
    let (cfg, paths, tx, id, upto) = (agent.cfg.clone(), agent.paths.clone(), tx.clone(), agent.log.id.clone(), *reviewed);
    tokio::spawn(async move {
        let _ = tx.send(Event::Learn { status: "learning from this session".into() });
        let gate = learn::gate(&cfg, &slice).await;
        learn::log_line(&paths, &format!("session {id} (live): gate {} {} -> {:?}", gate.source, gate.note, gate.kind));
        // The counters said "time to look"; the gate says whether there's anything to see.
        let kind = match (gate.kind, force) {
            (Some(k), _) => k,
            (None, true) => kind,
            (None, false) => {
                let mut st = learn::state(&paths);
                st["reviewed"][&id] = serde_json::json!(upto);
                learn::save_state(&paths, &st);
                let _ = tx.send(Event::Learn { status: "nothing new to learn".into() });
                return;
            }
        };
        let status = match learn::review(&cfg, &paths, &slice, kind).await {
            Ok(o) => {
                learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
                if o.actions.is_empty() { "learned nothing new".to_string() } else { format!("learned: {}", o.actions.iter().map(|a| a.split(':').next().unwrap_or("")).collect::<Vec<_>>().join(", ")) }
            }
            Err(e) => format!("learning failed: {e}"),
        };
        let mut st = learn::state(&paths);
        st["reviewed"][&id] = serde_json::json!(upto);
        learn::save_state(&paths, &st);
        let _ = tx.send(Event::Learn { status });
    });
}
