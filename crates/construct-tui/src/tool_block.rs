//! Rendering a `Kind::Tool` transcript block: header (with ticking `+N −M`
//! diff counters on `edit`), live streaming output tail whose lines arrive
//! bright and fade darker as newer lines scroll them up, smooth collapse from
//! a live tail down to a compact preview on completion, and expanded output.

use super::app::{Block, Status};
use super::motion::INK;
use super::text::fit;
use super::theme;
use construct_core::tools::OutputKind;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

/// Live rolling tail lines shown while a command is running.
pub(super) const LIVE_TAIL: usize = 5;
/// Settled compact preview lines for a command that streamed live output.
const SETTLED_LIVE: usize = 2;

/// Render `b` at width `w`. `diff_k` is the 0..1 tick-up progress for `+N −M`
/// counters; `collapse` is the current collapsing row count (`Some(n)` while a
/// live tail shrinks toward its compact preview).
pub(super) fn lines(
    b: &Block,
    w: usize,
    phase: f32,
    expand: bool,
    motion_on: bool,
    diff_k: f32,
    collapse: Option<usize>,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let (icon, color) = match b.status {
        Status::Running => (theme::spinner(phase).to_string(), theme::YELLOW),
        Status::Ok => ("✓".into(), theme::GREEN),
        Status::Err => ("✗".into(), theme::RED),
    };
    let diff_counts = (b.status == Status::Ok && b.out == OutputKind::Diff)
        .then(|| count_diff(&b.text))
        .filter(|&(p, m)| p > 0 || m > 0);
    let diff_spans = diff_counts.map(|(added, removed)| {
        let p = (added as f32 * diff_k).round() as usize;
        let m = (removed as f32 * diff_k).round() as usize;
        (format!(" +{p}"), format!(" −{m}"))
    });
    let time = match b.status {
        Status::Running => format!(" {:.1}s", b.started.elapsed().as_secs_f32()),
        _ if b.ms >= 1000 => format!(" {:.1}s", b.ms as f32 / 1000.0),
        _ => format!(" {}ms", b.ms),
    };
    let diff_w = diff_spans.as_ref().map_or(0, |(p, m)| p.width() + m.width());
    let room = w.saturating_sub(5 + b.title.width() + diff_w + time.width());
    let mut head = vec![
        Span::styled(format!("  {icon} "), Style::new().fg(color)),
        Span::styled(b.title.clone(), Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(fit(&b.detail, room), theme::dim()),
    ];
    if let Some((p, m)) = diff_spans {
        head.push(Span::styled(p, Style::new().fg(theme::GREEN)));
        head.push(Span::styled(m, Style::new().fg(theme::RED)));
    }
    head.push(Span::styled(time, theme::faint()));
    out.push(Line::from(head));

    if b.status == Status::Running {
        render_live(b, w, motion_on, &mut out);
        return out;
    }
    if b.text.is_empty() {
        return out;
    }
    match b.out {
        OutputKind::Tail => render_tail(b, w, expand, collapse, &mut out),
        OutputKind::Diff => render_diff(b, w, expand, &mut out),
        OutputKind::Plain => render_plain(b, w, expand, &mut out),
    }
    out
}

/// Settled body line target for a finished `Tail` block (used to drive collapse).
pub(super) fn settled_tail_rows(b: &Block, expand: bool) -> usize {
    if expand {
        60
    } else if b.status == Status::Err {
        4
    } else if b.had_live {
        SETTLED_LIVE
    } else {
        b.preview
    }
}

fn render_live(b: &Block, w: usize, motion_on: bool, out: &mut Vec<Line<'static>>) {
    let mut recent: Vec<(&str, Instant)> = b.live.iter().map(|(s, t)| (s.as_str(), *t)).collect();
    if !b.partial.is_empty() {
        recent.push((b.partial.as_str(), Instant::now()));
    }
    if recent.is_empty() {
        return;
    }
    let skip = recent.len().saturating_sub(LIVE_TAIL);
    let shown = &recent[skip..];
    let n = shown.len();
    for (idx, (line, arrived)) in shown.iter().enumerate() {
        let (gutter_style, text_style) = if motion_on {
            let age = arrived.elapsed().as_secs_f32();
            let rank = (n - 1 - idx) as f32;
            let cool = (age / 0.85).max(rank * 0.24).clamp(0.0, 1.0);
            let g = if idx + 1 == n {
                Style::new().fg(theme::lerp(theme::YELLOW, theme::FAINT, (age / 0.5).clamp(0.0, 1.0)))
            } else {
                theme::faint()
            };
            (g, Style::new().fg(theme::lerp(INK, theme::FAINT, cool)))
        } else {
            (theme::faint(), theme::faint())
        };
        out.push(Line::from(vec![
            Span::styled("    │ ", gutter_style),
            Span::styled(fit(&line.replace('\t', "  "), w.saturating_sub(6)), text_style),
        ]));
    }
}

fn render_tail(b: &Block, w: usize, expand: bool, collapse: Option<usize>, out: &mut Vec<Line<'static>>) {
    let all: Vec<&str> = b.text.lines().collect();
    let settled = settled_tail_rows(b, expand);
    let collapsing = !expand && collapse.is_some_and(|n| n > settled);
    let max = if collapsing { collapse.unwrap_or(settled) } else { settled };
    let take = all.len().min(max);
    let start = all.len().saturating_sub(take);
    for l in &all[start..] {
        let style = if b.status == Status::Err {
            Style::new().fg(theme::RED)
        } else if l.trim_start().starts_with("test result: ok") {
            Style::new().fg(theme::GREEN)
        } else {
            theme::faint()
        };
        out.push(Line::from(vec![
            Span::styled("    │ ", theme::faint()),
            Span::styled(fit(&l.replace('\t', "  "), w.saturating_sub(6)), style),
        ]));
    }
    if !collapsing && all.len() > take {
        out.push(Line::from(Span::styled(
            format!("    │ … {} more lines{}", all.len() - take, if expand { "" } else { " (ctrl+o)" }),
            theme::faint(),
        )));
    }
}

fn render_diff(b: &Block, w: usize, expand: bool, out: &mut Vec<Line<'static>>) {
    let max = if expand {
        60
    } else if b.status == Status::Err {
        4
    } else {
        b.preview
    };
    let lines: Vec<&str> = b.text.lines().skip(1).collect();
    for l in lines.iter().take(max) {
        let style = match l.chars().next() {
            Some('+') => Style::new().fg(theme::GREEN),
            Some('-') => Style::new().fg(theme::RED),
            _ => theme::faint(),
        };
        out.push(Line::from(vec![
            Span::styled("    │ ", theme::faint()),
            Span::styled(fit(&l.replace('\t', "  "), w.saturating_sub(6)), style),
        ]));
    }
    if lines.len() > max {
        out.push(Line::from(Span::styled(
            format!("    │ … {} more lines{}", lines.len() - max, if expand { "" } else { " (ctrl+o)" }),
            theme::faint(),
        )));
    }
}

fn render_plain(b: &Block, w: usize, expand: bool, out: &mut Vec<Line<'static>>) {
    let max = if expand {
        60
    } else if b.status == Status::Err {
        4
    } else {
        b.preview
    };
    let lines: Vec<&str> = b.text.lines().collect();
    for l in lines.iter().take(max) {
        let style = if b.status == Status::Err { Style::new().fg(theme::RED) } else { theme::faint() };
        out.push(Line::from(vec![
            Span::styled("    │ ", theme::faint()),
            Span::styled(fit(&l.replace('\t', "  "), w.saturating_sub(6)), style),
        ]));
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

/// Added and removed lines in a diff body (skipping the edit summary first line).
fn count_diff(out: &str) -> (usize, usize) {
    let (mut added, mut removed) = (0, 0);
    for l in out.lines().skip(1) {
        if l.starts_with('+') && !l.starts_with("+++") {
            added += 1;
        } else if l.starts_with('-') && !l.starts_with("---") {
            removed += 1;
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Kind;

    #[test]
    fn live_tail_fades_older_lines_darker_than_newest() {
        let mut b = Block::new(Kind::Tool, "");
        b.title = "bash".into();
        b.status = Status::Running;
        let now = Instant::now();
        b.live = vec![("line 1".into(), now), ("line 2".into(), now), ("line 3".into(), now)];
        let rendered = lines(&b, 80, 0.0, false, true, 1.0, None);
        assert_eq!(rendered.len(), 4, "header + 3 live lines");
        let top_fg = rendered[1].spans[1].style.fg;
        let bot_fg = rendered[3].spans[1].style.fg;
        assert_ne!(top_fg, bot_fg, "upper lines are darker than the newest bottom line");
    }

    #[test]
    fn edit_header_counts_added_and_removed_lines() {
        let mut b = Block::new(Kind::Tool, "edited a.rs\n context\n-old\n+new1\n+new2");
        b.title = "edit".into();
        b.detail = "a.rs".into();
        b.out = OutputKind::Diff;
        let rendered = lines(&b, 80, 0.0, false, false, 1.0, None);
        let head: String = rendered[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(head.contains("+2") && head.contains("−1"), "header has diff counts: {head:?}");
    }
}
