//! Rendering transcript blocks into styled lines, with per-block caching,
//! viewport slicing and motion row placement tracking.

use super::{Block, Kind, Status, Transcript};
use crate::motion::{Motion, Place};
use crate::{boxed, help, markdown, theme};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

impl Block {
    fn render_lines(
        &self,
        width: usize,
        phase: f32,
        expand: bool,
        motion_on: bool,
        diff_k: f32,
        collapse: Option<usize>,
    ) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        match &self.kind {
            Kind::User => {
                let bar = Span::styled("▌ ", Style::new().fg(theme::ACCENT2));
                for para in self.text.split('\n') {
                    let style = Style::new().add_modifier(Modifier::BOLD);
                    let lines = markdown::wrap(
                        vec![Span::styled(para.to_string(), style)],
                        width,
                        vec![bar.clone()],
                        vec![bar.clone()],
                    );
                    out.extend(lines);
                }
            }
            Kind::Assistant => {
                let md = markdown::render(&self.text, width.saturating_sub(2));
                for l in md {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(l.spans);
                    out.push(Line::from(spans));
                }
            }
            Kind::Reasoning => {
                let st = Style::new().fg(theme::DIM).add_modifier(Modifier::ITALIC);
                if self.status == Status::Running || expand {
                    let label = if self.status == Status::Running { "thinking" } else { "thought" };
                    let mut head = vec![Span::styled("  ✻ ", Style::new().fg(theme::ACCENT2))];
                    head.push(Span::styled(label.to_string(), theme::dim()));
                    out.push(Line::from(head));
                    let text = self.text.trim();
                    let lines: Vec<Line<'static>> = text
                        .split('\n')
                        .flat_map(|p| {
                            markdown::wrap(
                                vec![Span::styled(p.to_string(), st)],
                                width,
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
                        Span::styled(format!("thought for {:.1}s", self.ms as f32 / 1000.0), theme::faint()),
                    ]));
                }
            }
            Kind::Tool => out.extend(self.render_tool(width, phase, expand, motion_on, diff_k, collapse)),
            Kind::Help => out.extend(help::lines(width, &self.text)),
            Kind::Boxes(max) => out.extend(boxed::lines(width, *max, &self.boxes, &self.text)),
            Kind::Notice(color) => {
                let st = Style::new().fg(*color);
                // One wrapped paragraph per line, so multi-line notices (/status, /help) keep their breaks.
                for line in self.text.split('\n') {
                    out.extend(markdown::wrap(
                        vec![Span::styled(line.to_string(), st)],
                        width,
                        vec![Span::raw("  ")],
                        vec![Span::raw("    ")],
                    ));
                }
            }
        }
        out
    }

    fn needs_gap_after(&self, prev: &Block) -> bool {
        matches!(self.kind, Kind::User | Kind::Assistant)
            || (self.kind == Kind::Tool && !matches!(prev.kind, Kind::Tool))
    }

    fn is_settled_tool(&self) -> bool {
        self.kind == Kind::Tool && self.status != Status::Running
    }
}

impl Transcript {
    fn block_lines(&mut self, i: usize, width: u16, phase: f32, motion: &Motion) -> Vec<Line<'static>> {
        let expand = self.expand;
        let b = &self.blocks[i];
        let diff_k = motion.diff_progress(i);
        let collapse = b.is_settled_tool().then(|| motion.collapse_rows(i, b.settled_tail_rows(expand))).flatten();
        let dynamic = b.status == Status::Running || diff_k < 1.0 || collapse.is_some();
        if !dynamic
            && let Some((w, v, e, lines)) = self.cache.get(&i)
            && *w == width
            && *v == b.version
            && *e == expand
        {
            return lines.clone();
        }
        let mut out = b.render_lines(width as usize, phase, expand, motion.on(), diff_k, collapse);
        // spacing: a blank line before user turns and assistant text
        if i > 0 && b.needs_gap_after(&self.blocks[i - 1]) {
            out.insert(0, Line::default());
        }
        if !dynamic {
            self.cache.insert(i, (width, b.version, expand, out.clone()));
        }
        out
    }

    /// Render the visible transcript window for `width` x `main_h` (or slice a
    /// pre-built splash body when the transcript is empty), updating scroll and
    /// returning `(visible_lines, first_line_index, motion_places)`.
    pub(crate) fn frame(
        &mut self,
        width: u16,
        main_h: u16,
        phase: f32,
        motion: &mut Motion,
        splash: Option<(Vec<Line<'static>>, Vec<Place>)>,
    ) -> (Vec<Line<'static>>, usize, Vec<Place>) {
        let (body, places) = match splash {
            Some(s) => s,
            None => {
                let mut lines = Vec::new();
                self.rows.clear();
                for i in 0..self.blocks.len() {
                    let mut b = self.block_lines(i, width.saturating_sub(1), phase, motion);
                    motion.clip(i, &mut b); // an output still opening
                    let gap = usize::from(b.first().is_some_and(|l| l.spans.is_empty())); // the blank line before a block
                    let w = b.iter().map(Line::width).max().unwrap_or(0) as u16;
                    self.rows.push(Place { left: 0, start: lines.len() + gap, len: b.len() - gap, width: w });
                    lines.extend(b);
                }
                (lines, self.rows.clone())
            }
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
        (body[start..end].to_vec(), start, places)
    }
}
