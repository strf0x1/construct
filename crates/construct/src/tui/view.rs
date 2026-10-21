//! Rendering: transcript blocks, splash, input line, activity and status
//! bars, and the frame layout.

use super::app::{App, Kind, Phase, Status};
use super::{markdown, theme};
use crate::tools::OutputKind;
use anyhow::Result;
use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::{Stdout, Write};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

impl App {
    pub(super) fn block_lines(&mut self, i: usize, width: u16, phase: f32) -> Vec<Line<'static>> {
        let expand = self.expand;
        let b = &self.blocks[i];
        let dynamic = b.status == Status::Running;
        if !dynamic
            && let Some((w, v, e, lines)) = self.cache.get(&i)
            && *w == width
            && *v == b.version
            && *e == expand
        {
            return lines.clone();
        }
        let w = width as usize;
        let mut out: Vec<Line<'static>> = Vec::new();
        match &b.kind {
            Kind::User => {
                let bar = Span::styled("▌ ", Style::new().fg(theme::ACCENT2));
                for (n, para) in b.text.split('\n').enumerate() {
                    let style = Style::new().add_modifier(Modifier::BOLD);
                    let lines = markdown::wrap(
                        vec![Span::styled(para.to_string(), style)],
                        w,
                        vec![bar.clone()],
                        vec![bar.clone()],
                    );
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
                        .flat_map(|p| {
                            markdown::wrap(
                                vec![Span::styled(p.to_string(), st)],
                                w,
                                vec![Span::raw("    ")],
                                vec![Span::raw("    ")],
                            )
                        })
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
                    let max = if expand {
                        60
                    } else if b.status == Status::Err {
                        4
                    } else {
                        b.preview
                    };
                    let body = tool_body(b.out, &b.text);
                    let lines: Vec<&str> = body.lines().collect();
                    for l in lines.iter().take(max) {
                        let style = if b.out == OutputKind::Diff {
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
                        out.push(Line::from(vec![
                            Span::styled("    │ ", theme::faint()),
                            Span::styled(fit(&l.replace('\t', "  "), w.saturating_sub(6)), style),
                        ]));
                    }
                    if max == 0 && !expand {
                        out.push(Line::from(Span::styled(format!("    │ {} lines", lines.len()), theme::faint())));
                    } else if lines.len() > max {
                        out.push(Line::from(Span::styled(
                            format!(
                                "    │ … {} more lines{}",
                                lines.len() - max,
                                if expand { "" } else { " (ctrl+o)" }
                            ),
                            theme::faint(),
                        )));
                    }
                }
            }
            Kind::Notice(color) => {
                let st = Style::new().fg(*color);
                out.extend(markdown::wrap(
                    vec![Span::styled(b.text.clone(), st)],
                    w,
                    vec![Span::raw("  ")],
                    vec![Span::raw("    ")],
                ));
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

    pub(super) fn transcript(&mut self, width: u16, phase: f32) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for i in 0..self.blocks.len() {
            lines.extend(self.block_lines(i, width, phase));
        }
        lines
    }

    pub(super) fn splash(&self, area: Rect, phase: f32) -> Vec<Line<'static>> {
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
                Span::styled(
                    c.to_string(),
                    Style::new()
                        .fg(theme::lerp(theme::ACCENT, theme::ACCENT2, i as f32 / 10.0))
                        .add_modifier(Modifier::BOLD),
                )
            }));
        }
        lines.push(Line::from(t));
        lines.push(Line::from(Span::styled("  lean · fast · learns as it goes", theme::dim())));
        lines.push(Line::default());
        let row = |k: &str, v: String| {
            Line::from(vec![Span::styled(format!("  {k:<9}"), theme::faint()), Span::styled(v, theme::dim())])
        };
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

    pub(super) fn input_lines(&self, width: usize) -> (Vec<Line<'static>>, (u16, u16)) {
        let prompt_w = 2;
        let avail = width.saturating_sub(prompt_w + 1).max(4);
        let mut lines = vec![];
        let mut cur = (0u16, prompt_w as u16);
        let mut row = 0u16;
        let mut offset = 0usize;
        let placeholder = self.input.is_empty();
        let text = if placeholder {
            if self.busy { "queue a message… (esc to interrupt)" } else { "ask construct anything… (/help)" }
                .to_string()
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
                let prefix = if pi == 0 && ci == 0 {
                    Span::styled("› ", Style::new().fg(theme::ACCENT2).add_modifier(Modifier::BOLD))
                } else {
                    Span::raw("  ")
                };
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

    pub(super) fn activity(&self, phase: f32) -> Line<'static> {
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

    pub(super) fn status(&self, width: usize, phase: f32) -> Line<'static> {
        let left = vec![
            Span::styled(" ◆ ", Style::new().fg(theme::ACCENT2)),
            Span::styled(self.cfg.model.clone(), theme::dim()),
        ];
        let mut right: Vec<Span<'static>> = vec![];
        if let Some((s, _)) = &self.learning {
            right.push(Span::styled(
                format!("◇ {s}  "),
                Style::new().fg(theme::pulse(phase, theme::FAINT, theme::ACCENT2)),
            ));
        } else if let Some((t, c, at)) = &self.toast
            && at.elapsed() < Duration::from_secs(6)
        {
            right.push(Span::styled(format!("◇ {}  ", crate::llm::truncate(t, 60)), Style::new().fg(*c)));
        }
        if !self.groups.is_empty() {
            right.push(Span::styled(format!("{}  ", self.groups.join(" ")), theme::faint()));
        }
        right.push(Span::styled(format!("{} tok", human(self.tokens)), theme::faint()));
        if self.context_pct > 0 {
            right.push(Span::styled(
                format!(" · ctx {}%", self.context_pct),
                if self.context_pct > 70 { Style::new().fg(theme::YELLOW) } else { theme::faint() },
            ));
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

    pub(super) fn draw(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let phase = self.started.elapsed().as_secs_f32();
        let size = term.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let width = area.width;
        let (input, cursor) = self.input_lines(width as usize);
        let input_h = (input.len() as u16).clamp(1, 8);
        let bottom = input_h + 3; // activity + rule + input + status
        let main_h = area.height.saturating_sub(bottom);
        let body = if self.blocks.is_empty() {
            self.splash(area, phase)
        } else {
            self.transcript(width.saturating_sub(1), phase)
        };
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
            f.render_widget(
                Paragraph::new(input.clone()).scroll((input_scroll, 0)),
                Rect::new(0, main_h + 2, width, input_h),
            );
            f.render_widget(Paragraph::new(status.clone()), Rect::new(0, main_h + 2 + input_h, width, 1));
            let cy = cursor.0.saturating_sub(input_scroll).min(input_h - 1);
            f.set_cursor_position(Position::new(cursor.1.min(width - 1), main_h + 2 + cy));
        })?;
        queue!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;
        Ok(())
    }
}

/// What to show under a finished tool: the diff under an edit's summary
/// line, the tail of command output.
fn tool_body(kind: OutputKind, out: &str) -> String {
    match kind {
        OutputKind::Diff => out.lines().skip(1).collect::<Vec<_>>().join("\n"),
        OutputKind::Tail => {
            let lines: Vec<&str> = out.lines().collect();
            if lines.len() > 4 {
                lines[lines.len() - 4..].join("\n") + &format!("\n({} lines)", lines.len())
            } else {
                out.to_string()
            }
        }
        OutputKind::Plain => out.to_string(),
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
