//! Motion that moves layout (ADR 0066): a finished tool's output opens row by
//! row (or collapses smoothly from a live streaming tail down to its compact
//! preview), toggling `ctrl+o` eases tool details open and closed, edit `+N −M`
//! counters tick up, and the permission panel rises out of the status line and
//! closes its room after its ghost dissolves. Each is a row count eased over a
//! few hundred milliseconds, read while the frame is laid out. With motion off
//! every count is the full one.

use super::Motion;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::{Duration, Instant};

/// How long a non-streaming tool's output takes to open.
pub(super) const OPEN: Duration = Duration::from_millis(900);
/// Brief hold after a live command ends before its tail collapses, so the
/// green ✓ flash registers first (as in motion-lab).
const SETTLE_HOLD: Duration = Duration::from_millis(120);
/// How long a live streaming tail takes to collapse into its settled preview.
pub(super) const SETTLE: Duration = Duration::from_millis(540);
/// How long a `ctrl+o` expand/collapse takes to ease between heights.
const TOGGLE: Duration = Duration::from_millis(320);
/// How long an edit header's `+N −M` counters take to tick up.
const DIFF_COUNT: Duration = Duration::from_millis(750);
/// How long the permission panel takes to rise.
const RISE: Duration = Duration::from_millis(320);
/// How long the panel's room takes to close once its ghost is gone.
const SINK: Duration = Duration::from_millis(300);
/// The ghost's life (`shaders::ghost`): the room is held this long first.
pub(super) const GHOST_MS: u32 = 420;

/// One block whose body row count is easing from `from` toward its target.
pub(super) struct Resize {
    pub(super) block: usize,
    pub(super) from: usize,
    pub(super) since: Instant,
    pub(super) dur: Duration,
    /// When collapsing a live tail (`from > target`), hold the older tail rows
    /// while shrinking so the renderer can keep showing them until they fold.
    pub(super) collapse: bool,
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn progress(since: Instant, d: Duration) -> f32 {
    since.elapsed().as_secs_f32() / d.as_secs_f32()
}

impl Motion {
    /// Tool block `i` finished without streaming: open its output from the header down.
    pub(super) fn open(&mut self, i: usize) {
        self.set_resize(Resize { block: i, from: 0, since: Instant::now(), dur: OPEN, collapse: false });
    }

    /// Tool block `i` finished after showing `from` live tail rows: smoothly
    /// collapse down to its settled preview.
    pub(super) fn collapse(&mut self, i: usize, from: usize) {
        self.set_resize(Resize { block: i, from, since: Instant::now(), dur: SETTLE, collapse: true });
    }

    /// The user toggled `ctrl+o`: ease each block from its previous body row count.
    pub(crate) fn toggle_expand(&mut self, heights: Vec<(usize, usize)>) {
        if !self.on || heights.is_empty() {
            return;
        }
        self.wake();
        let now = Instant::now();
        for (block, from) in heights {
            self.set_resize(Resize { block, from, since: now, dur: TOGGLE, collapse: false });
        }
    }

    /// Record an edit whose `+N −M` header counters should tick up.
    pub(super) fn tick_diff(&mut self, i: usize) {
        self.diff_tick.retain(|(b, _)| *b != i);
        self.diff_tick.push((i, Instant::now()));
    }

    /// 0..1 progress for block `i`'s diff `+N −M` counters (`1.0` when settled or off).
    pub(crate) fn diff_progress(&self, i: usize) -> f32 {
        if !self.on {
            return 1.0;
        }
        self.diff_tick
            .iter()
            .find(|(b, _)| *b == i)
            .filter(|(_, s)| s.elapsed() < DIFF_COUNT)
            .map_or(1.0, |(_, s)| ease_out(progress(*s, DIFF_COUNT)))
    }

    /// If block `i` is collapsing from a live tail down toward `target` rows,
    /// return how many tail rows to render this frame (strictly greater than
    /// `target`), or `None` once it has reached the settled preview.
    pub(crate) fn collapse_rows(&self, i: usize, target: usize) -> Option<usize> {
        if !self.on {
            return None;
        }
        let r = self.resize.iter().find(|r| r.block == i && r.collapse && r.since.elapsed() < r.dur)?;
        if r.from <= target {
            return None;
        }
        let elapsed = r.since.elapsed();
        let active = r.dur.saturating_sub(SETTLE_HOLD);
        let t = elapsed.checked_sub(SETTLE_HOLD).map_or(0.0, |e| e.as_secs_f32() / active.as_secs_f32());
        let k = ease_out(t);
        let shown = (r.from as f32 - (r.from - target) as f32 * k).ceil() as usize;
        (shown > target).then_some(shown)
    }

    fn set_resize(&mut self, r: Resize) {
        self.resize.retain(|x| x.block != r.block);
        self.resize.push(r);
    }

    /// Cut block `i`'s lines to what has eased in so far: the blank line before
    /// it and its header always, then the body interpolated from `from` to
    /// `target`.
    pub(crate) fn clip(&mut self, i: usize, lines: &mut Vec<Line<'static>>) {
        self.resize.retain(|r| r.since.elapsed() < r.dur);
        self.diff_tick.retain(|(_, s)| s.elapsed() < DIFF_COUNT);
        let Some(r) = self.resize.iter().find(|r| r.block == i) else { return };
        if r.collapse {
            // Tail collapse is driven directly in `tool_block` via `collapse_rows`.
            return;
        }
        let head = 1 + usize::from(lines.first().is_some_and(|l| l.spans.is_empty()));
        let target = lines.len().saturating_sub(head);
        let k = ease_out(progress(r.since, r.dur));
        let shown = if target >= r.from {
            r.from + ((target - r.from) as f32 * k).ceil() as usize
        } else {
            r.from - ((r.from - target) as f32 * k).ceil() as usize
        };
        if shown < target {
            lines.truncate(head + shown);
        }
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

    /// Layout or counters are still moving.
    pub(super) fn moving(&self) -> bool {
        self.resize.iter().any(|r| r.since.elapsed() < r.dur)
            || self.diff_tick.iter().any(|(_, s)| s.elapsed() < DIFF_COUNT)
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
        m.resize[0].since = Instant::now() - OPEN / 2;
        let mut half = block(10);
        m.clip(3, &mut half);
        assert!((8..12).contains(&half.len()), "eased: most of it by half time ({})", half.len());
        m.resize[0].since = Instant::now() - OPEN;
        let mut done = block(10);
        m.clip(3, &mut done);
        assert_eq!(done.len(), 12);
        assert!(m.resize.is_empty());
    }

    #[test]
    fn a_live_tail_collapses_smoothly_to_preview() {
        let mut m = Motion::new(true);
        m.collapse(1, 5);
        assert_eq!(m.collapse_rows(1, 2), Some(5));
        m.resize[0].since = Instant::now() - SETTLE / 2;
        let mid = m.collapse_rows(1, 2);
        assert!(mid.is_some_and(|n| (2..5).contains(&n)), "shrinking mid-collapse: {mid:?}");
        m.resize[0].since = Instant::now() - SETTLE;
        assert_eq!(m.collapse_rows(1, 2), None);
    }

    #[test]
    fn toggle_expand_eases_from_old_height() {
        let mut m = Motion::new(true);
        m.toggle_expand(vec![(0, 2)]);
        let mut lines = block(12);
        m.clip(0, &mut lines);
        assert!((4..8).contains(&lines.len()), "starts near previous height (2 + head): {}", lines.len());
    }

    #[test]
    fn diff_counters_tick_up_and_settle() {
        let mut m = Motion::new(true);
        m.tick_diff(4);
        assert!(m.diff_progress(4) < 0.5);
        m.diff_tick[0].1 = Instant::now() - DIFF_COUNT;
        assert_eq!(m.diff_progress(4), 1.0);
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
        assert_eq!(m.diff_progress(0), 1.0);
        assert_eq!(m.collapse_rows(0, 2), None);
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
