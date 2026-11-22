//! Motion that moves layout (ADR 0066): a finished tool's output opens row by
//! row, pushing the transcript up, and the permission panel rises out of the
//! status line and, once answered, the room it held closes after its ghost
//! has dissolved. Each is a row count eased over a few hundred milliseconds,
//! asked for while the frame is laid out, so the transcript's bottom anchor
//! and scroll keep working as they do for any block that changes height.
//! With motion off every count is the full one.

use super::Motion;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::{Duration, Instant};

/// How long a tool's output takes to open.
pub(super) const OPEN: Duration = Duration::from_millis(900);
/// How long the permission panel takes to rise.
const RISE: Duration = Duration::from_millis(320);
/// How long the panel's room takes to close once its ghost is gone.
const SINK: Duration = Duration::from_millis(300);
/// The ghost's life (`shaders::ghost`): the room is held this long first.
pub(super) const GHOST_MS: u32 = 420;

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn progress(since: Instant, d: Duration) -> f32 {
    since.elapsed().as_secs_f32() / d.as_secs_f32()
}

impl Motion {
    /// Tool block `i` finished: open its output from the header down.
    pub(super) fn open(&mut self, i: usize) {
        self.opening.retain(|(b, _)| *b != i);
        self.opening.push((i, Instant::now()));
    }

    /// Cut block `i`'s lines to what has opened so far: the blank line
    /// before it and its header always, then the body by an eased share.
    pub(crate) fn clip(&mut self, i: usize, lines: &mut Vec<Line<'static>>) {
        self.opening.retain(|(_, s)| s.elapsed() < OPEN);
        let Some((_, since)) = self.opening.iter().find(|(b, _)| *b == i) else { return };
        let head = 1 + usize::from(lines.first().is_some_and(|l| l.spans.is_empty()));
        let body = lines.len().saturating_sub(head);
        let shown = (ease_out(progress(*since, OPEN)) * body as f32).ceil() as usize;
        lines.truncate(head + shown);
    }

    /// The rows the permission panel takes this frame, out of `h`. A new
    /// question starts from none and rises over `RISE`.
    pub(crate) fn panel_rows(&mut self, h: u16) -> u16 {
        if !self.on {
            return h;
        }
        if (self.panel.is_none() || self.rearm) && self.rise.is_none() {
            self.wake();
            self.rise = Some(Instant::now());
            self.sink = None;
        }
        let Some(since) = self.rise else { return h };
        let k = ease_out(progress(since, RISE));
        if k >= 1.0 {
            self.rise = None;
            return h;
        }
        ((k * h as f32).round() as u16).clamp(1, h)
    }

    /// Rows to keep empty under the transcript while an answered panel's
    /// room closes: all of it while the ghost dissolves, then less and less.
    /// `bottom` is what the screen's foot takes without the panel.
    pub(crate) fn reserve(&mut self, bottom: u16) -> u16 {
        let Some((h, since)) = self.sink else { return 0 };
        let room = (h + 1).saturating_sub(bottom);
        let held = Duration::from_millis(u64::from(GHOST_MS));
        let k = since.elapsed().checked_sub(held).map_or(0.0, |t| ease_out(t.as_secs_f32() / SINK.as_secs_f32()));
        if k >= 1.0 {
            self.sink = None;
        }
        ((1.0 - k) * room as f32).round() as u16
    }

    /// The answered panel's room starts closing (held while its ghost runs).
    pub(super) fn close_room(&mut self) {
        self.sink = self.panel.as_ref().map(|p| (p.area.height, Instant::now()));
    }

    /// Layout is still moving.
    pub(super) fn moving(&self) -> bool {
        self.opening.iter().any(|(_, s)| s.elapsed() < OPEN)
            || self.rise.is_some()
            || self.sink.is_some_and(|(_, s)| s.elapsed() < SINK + Duration::from_millis(u64::from(GHOST_MS)))
    }
}

/// The panel was drawn whole over `full`; show only its top `rows` rows, at
/// the foot of `full`, as if it were rising out from under the status line.
/// Rows above them go back to empty, for the transcript to draw on.
pub(crate) fn rise(buf: &mut Buffer, full: Rect, rows: u16) {
    let full = full.intersection(buf.area);
    if rows >= full.height {
        return;
    }
    let top: Vec<Cell> = (full.y..full.y + rows)
        .flat_map(|y| (full.x..full.right()).map(move |x| (x, y)))
        .map(|p| buf[p].clone())
        .collect();
    for p in full.positions() {
        buf[p].reset();
    }
    let at = full.bottom() - rows;
    for (i, c) in top.into_iter().enumerate() {
        let (dx, dy) = ((i % full.width as usize) as u16, (i / full.width as usize) as u16);
        buf[(full.x + dx, at + dy)] = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn block(body: usize) -> Vec<Line<'static>> {
        let mut v = vec![Line::default(), Line::from(Span::raw("  ✓ bash"))];
        v.extend((0..body).map(|n| Line::from(Span::raw(format!("    │ {n}")))));
        v
    }

    #[test]
    fn a_body_opens_from_its_header_down() {
        let mut m = Motion::new(true);
        m.open(3);
        let mut first = block(10);
        m.clip(3, &mut first);
        assert!(first.len() <= 3, "gap and header, a row at most, at first ({})", first.len());
        let mut other = block(10);
        m.clip(2, &mut other);
        assert_eq!(other.len(), 12, "other blocks are whole");
        m.opening[0].1 = Instant::now() - OPEN / 2;
        let mut half = block(10);
        m.clip(3, &mut half);
        assert!((8..12).contains(&half.len()), "eased: most of it by half time ({})", half.len());
        m.opening[0].1 = Instant::now() - OPEN;
        let mut done = block(10);
        m.clip(3, &mut done);
        assert_eq!(done.len(), 12);
        assert!(m.opening.is_empty());
    }

    #[test]
    fn the_panel_rises_then_its_room_closes_after_the_ghost() {
        let mut m = Motion::new(true);
        assert!(m.panel_rows(9) < 3, "a new question starts low");
        m.rise = m.rise.map(|_| Instant::now() - RISE);
        assert_eq!(m.panel_rows(9), 9);
        m.panel = Some(super::super::panel::Panel::for_test(Rect::new(0, 10, 40, 9)));
        assert_eq!(m.panel_rows(9), 9, "an open panel stays up");
        m.close_room();
        assert_eq!(m.reserve(4), 6, "the room is held while the ghost runs");
        m.sink = m.sink.map(|(h, _)| (h, Instant::now() - SINK - Duration::from_millis(u64::from(GHOST_MS))));
        assert_eq!(m.reserve(4), 0);
        assert!(m.sink.is_none());
    }

    #[test]
    fn off_takes_full_rows() {
        let mut m = Motion::new(false);
        assert_eq!(m.panel_rows(9), 9);
        assert_eq!(m.reserve(4), 0);
    }

    #[test]
    fn a_rising_panel_shows_its_top_at_the_foot() {
        let mut b = Buffer::empty(Rect::new(0, 0, 6, 6));
        for y in 2..6 {
            b.set_string(0, y, format!("row{y}"), ratatui::style::Style::new());
        }
        rise(&mut b, Rect::new(0, 2, 6, 4), 2);
        assert_eq!(b[(0, 2)].symbol(), " ");
        assert_eq!(b[(3, 4)].symbol(), "2", "the panel's first row, two up from the foot");
        assert_eq!(b[(3, 5)].symbol(), "3");
    }
}
