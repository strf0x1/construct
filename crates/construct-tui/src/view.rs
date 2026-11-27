//! Rendering: transcript blocks, splash, input line, status bar, and the
//! frame layout. The activity line is `activity.rs`.

use super::app::{App, Kind, Status};
use super::motion::{Place, rise};
use super::{boxed, help, markdown, prompt, theme, tool_block};
use anyhow::Result;
use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::{Stdout, Write};

impl App {
    pub(super) fn block_lines(&mut self, i: usize, width: u16, phase: f32) -> Vec<Line<'static>> {
        let expand = self.expand;
        let b = &self.blocks[i];
        let diff_k = self.motion.diff_progress(i);
        let collapse = (b.kind == Kind::Tool && b.status != Status::Running)
            .then(|| self.motion.collapse_rows(i, tool_block::settled_tail_rows(b, expand)))
            .flatten();
        let dynamic = b.status == Status::Running || diff_k < 1.0 || collapse.is_some();
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
                for para in b.text.split('\n') {
                    let style = Style::new().add_modifier(Modifier::BOLD);
                    let lines = markdown::wrap(
                        vec![Span::styled(para.to_string(), style)],
                        w,
                        vec![bar.clone()],
                        vec![bar.clone()],
                    );
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
            Kind::Tool => out.extend(tool_block::lines(b, w, phase, expand, self.motion.on(), diff_k, collapse)),
            Kind::Help => out.extend(help::lines(w, &b.text)),
            Kind::Boxes(max) => out.extend(boxed::lines(w, *max, &b.boxes, &b.text)),
            Kind::Notice(color) => {
                let st = Style::new().fg(*color);
                // One wrapped paragraph per line, so multi-line notices (/status, /help) keep their breaks.
                for line in b.text.split('\n') {
                    out.extend(markdown::wrap(
                        vec![Span::styled(line.to_string(), st)],
                        w,
                        vec![Span::raw("  ")],
                        vec![Span::raw("    ")],
                    ));
                }
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
        self.rows.clear();
        for i in 0..self.blocks.len() {
            let mut b = self.block_lines(i, width, phase);
            self.motion.clip(i, &mut b); // an output still opening
            let gap = usize::from(b.first().is_some_and(|l| l.spans.is_empty())); // the blank line before a block
            let w = b.iter().map(Line::width).max().unwrap_or(0) as u16;
            self.rows.push(Place { left: 0, start: lines.len() + gap, len: b.len() - gap, width: w });
            lines.extend(b);
        }
        lines
    }

    pub(super) fn draw(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let phase = self.started.elapsed().as_secs_f32();
        let size = term.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let width = area.width;
        self.prune_prompts();
        let (input, cursor) = self.input_lines(width as usize);
        let input_h = (input.len() as u16).clamp(1, 8);
        // The slash palette sits between the input and the status line, and
        // gives way to the transcript on a short terminal.
        let rows = (area.height.saturating_sub(input_h + 6) / 2) as usize;
        let palette = self.palette.as_ref().map(|p| p.lines(width as usize, rows)).unwrap_or_default();
        let palette_h = palette.len() as u16;
        // A question takes the place of the activity line, rule, input and palette.
        let panel_h = self.prompts.front().map(|p| prompt::height(p, width, area.height.saturating_sub(4)));
        let risen = panel_h.map(|h| self.motion.panel_rows(h)); // rows it shows while rising
        let bottom = match risen {
            Some(h) => h + 1, // panel + status
            None => {
                let foot = input_h + palette_h + 3; // activity + rule + input + palette + status
                foot + self.motion.reserve(foot) // an answered panel's room, closing
            }
        };
        let main_h = area.height.saturating_sub(bottom);
        let is_splash = self.blocks.is_empty();
        let body =
            if is_splash { self.splash(width, main_h, phase) } else { self.transcript(width.saturating_sub(1), phase) };
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
        let rule_color = self.motion.rule_color(self.turn.busy(), phase);
        let mut motion = std::mem::take(&mut self.motion); // the draw closure reads the rest of self
        motion.turn(self.turn.busy().then(|| self.turn.running().map(|r| &r.phase)));
        let mut stdout = std::io::stdout();
        queue!(stdout, BeginSynchronizedUpdate)?;
        term.draw(|f| {
            // pad from the top so short transcripts sit right above the input;
            // the splash centers itself vertically inside `main_h`.
            let pad = if is_splash { 0 } else { main_h.saturating_sub(visible.len() as u16) };
            // a rising panel is drawn whole, then cut to its top rows at the foot
            let panel = panel_h.zip(risen).zip(self.prompts.front());
            if let Some(((h, shown), p)) = panel {
                let full = Rect::new(0, area.height.saturating_sub(h + 1), width, h);
                prompt::render(f, full, p, self.prompts.len() - 1, phase);
                motion.panel(f.buffer_mut(), full);
                rise(f.buffer_mut(), full, shown);
            }
            f.render_widget(Paragraph::new(visible.clone()), Rect::new(0, pad, width, main_h - pad));
            motion.transcript(f.buffer_mut(), &self.rows, start, pad, main_h - pad, is_splash);
            super::peers::draw(f, &mut self.peers, Rect::new(0, 0, width, main_h), self.prompts.is_empty(), phase);
            if panel.is_some() {
                f.render_widget(Paragraph::new(status.clone()), Rect::new(0, area.height.saturating_sub(1), width, 1));
                return;
            }
            let low = area.height.saturating_sub(input_h + palette_h + 3);
            f.render_widget(Paragraph::new(activity.clone()), Rect::new(0, low, width, 1));
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("─".repeat(width as usize), Style::new().fg(rule_color)))),
                Rect::new(0, low + 1, width, 1),
            );
            f.render_widget(
                Paragraph::new(input.clone()).scroll((input_scroll, 0)),
                Rect::new(0, low + 2, width, input_h),
            );
            f.render_widget(Paragraph::new(palette.clone()), Rect::new(0, low + 2 + input_h, width, palette_h));
            f.render_widget(Paragraph::new(status.clone()), Rect::new(0, low + 2 + input_h + palette_h, width, 1));
            motion.rule(f.buffer_mut(), Rect::new(0, low + 1, width, 1)); // last: a dissolving panel lies over all
            let cy = cursor.0.saturating_sub(input_scroll).min(input_h - 1);
            f.set_cursor_position(Position::new(cursor.1.min(width - 1), low + 2 + cy));
            self.render_modal(f, Rect::new(0, 0, width, area.height.saturating_sub(1)));
        })?;
        self.motion = motion;
        queue!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;
        Ok(())
    }
}
