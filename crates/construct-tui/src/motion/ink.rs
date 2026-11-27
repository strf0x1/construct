//! Streaming text dries: the newest characters of the reply being written
//! land bright and cool to their own color over `DRY`, and a soft caret
//! blinks at the head while the model writes. Text arrives in bursts, so
//! each burst's characters are spread over the time since the one before
//! (at most `SPREAD`) and the tail reads as a stream, not as blocks. A
//! character is matched to a drawn cell by counting non-blank cells back
//! from the end, so markdown that hides its syntax shifts the match by a
//! cell or two at most, which nobody sees in a fade.

use super::{INK, Motion, WHITE};
use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::time::{Duration, Instant};

/// How long a character takes to dry.
const DRY: Duration = Duration::from_millis(600);
/// The longest a burst is spread over.
const SPREAD: Duration = Duration::from_millis(140);

/// The reply being written: its block and when its characters arrived, as
/// (time, non-blank characters so far).
pub(super) struct Ink {
    pub(super) block: usize,
    log: Vec<(Instant, usize)>,
}

impl Ink {
    fn total(&self) -> usize {
        self.log.last().map_or(0, |l| l.1)
    }

    /// When non-blank character `c` (from 0) arrived.
    fn arrival(&self, c: usize) -> Option<Instant> {
        let j = self.log.iter().position(|l| l.1 > c)?;
        let (t1, n1) = self.log[j];
        let (t0, n0) = j.checked_sub(1).map_or((t1, 0), |p| self.log[p]);
        let start = t0.max(t1.checked_sub(SPREAD).unwrap_or(t1));
        let frac = (c + 1 - n0) as f32 / (n1 - n0).max(1) as f32;
        Some(start + (t1 - start).mul_f32(frac))
    }
}

impl Motion {
    /// Block `i`, the reply being written, grew by `text`.
    pub(crate) fn wrote(&mut self, i: usize, text: &str) {
        if !self.on {
            return;
        }
        let added = text.chars().filter(|c| !c.is_whitespace()).count();
        let now = Instant::now();
        let ink = match self.ink.as_mut() {
            Some(ink) if ink.block == i => ink,
            _ => {
                self.wake();
                let start = now.checked_sub(SPREAD).unwrap_or(now);
                self.ink.insert(Ink { block: i, log: vec![(start, 0)] })
            }
        };
        let total = ink.total() + added;
        ink.log.push((now, total));
        // keep one entry older than anything still drying, as the baseline
        let old = ink.log.iter().rposition(|l| now.duration_since(l.0) > DRY + SPREAD);
        if let Some(k) = old.filter(|k| *k > 0) {
            ink.log.drain(..k);
        }
    }

    /// Something is still drying, or the caret blinks.
    pub(super) fn drying(&self) -> bool {
        self.ink.as_ref().is_some_and(|ink| self.writing || ink.log.last().is_some_and(|l| l.0.elapsed() < DRY))
    }

    /// Dry the reply drawn over `rect`, and put the caret after its last
    /// character while the model is writing.
    pub(super) fn dry(&mut self, buf: &mut Buffer, block: usize, rect: Option<Rect>) {
        let Some(ink) = self.ink.as_ref().filter(|ink| ink.block == block) else { return };
        let Some(rect) = rect else { return };
        let now = Instant::now();
        let total = ink.total();
        let mut seen = 0;
        let mut head = None;
        'rows: for y in (rect.y..rect.bottom()).rev() {
            for x in (rect.x..rect.right()).rev() {
                let cell = &mut buf[(x, y)];
                if cell.symbol().trim().is_empty() {
                    continue;
                }
                head.get_or_insert((x, y));
                let Some(c) = total.checked_sub(seen + 1) else { break 'rows };
                seen += 1;
                let age = ink.arrival(c).map_or(DRY, |t| now.saturating_duration_since(t));
                if age >= DRY {
                    break 'rows;
                }
                let k = age.as_secs_f32() / DRY.as_secs_f32();
                let to = if matches!(cell.fg, Color::Rgb(..)) { cell.fg } else { INK };
                cell.set_fg(theme::lerp(WHITE, to, k));
            }
        }
        if self.writing
            && let Some((x, y)) = head
            && x + 1 < rect.right()
        {
            let blink = (self.started.elapsed().as_secs_f32() * 3.0).fract();
            buf[(x + 1, y)].set_symbol("▍").set_fg(theme::lerp(theme::ACCENT, theme::FAINT, blink));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn the_newest_text_is_bright_and_older_text_is_left_alone() {
        let mut m = Motion::new(true);
        m.wrote(0, "old words");
        if let Some(i) = m.ink.as_mut() {
            i.log.iter_mut().for_each(|l| l.0 -= DRY * 2)
        }
        m.wrote(0, " new");
        let mut b = Buffer::empty(Rect::new(0, 0, 20, 1));
        b.set_string(2, 0, "old words new", Style::new().fg(theme::SOFT));
        m.dry(&mut b, 0, Some(Rect::new(0, 0, 20, 1)));
        assert_ne!(b[(14, 0)].fg, theme::SOFT, "the last character is still bright");
        assert_eq!(b[(2, 0)].fg, theme::SOFT, "the first has dried");
    }

    #[test]
    fn the_caret_follows_the_text_while_writing() {
        let mut m = Motion::new(true);
        m.writing = true;
        m.wrote(1, "hi");
        let mut b = Buffer::empty(Rect::new(0, 0, 10, 2));
        b.set_string(2, 0, "hi", Style::new());
        m.dry(&mut b, 1, Some(Rect::new(0, 0, 10, 2)));
        assert_eq!(b[(4, 0)].symbol(), "▍");
        m.writing = false;
        let mut b = Buffer::empty(Rect::new(0, 0, 10, 2));
        b.set_string(2, 0, "hi", Style::new());
        m.dry(&mut b, 1, Some(Rect::new(0, 0, 10, 2)));
        assert_eq!(b[(4, 0)].symbol(), " ", "no caret once writing stops");
    }

    #[test]
    fn a_burst_is_spread_out() {
        let mut m = Motion::new(true);
        m.wrote(0, "abcdefghij");
        let ink = m.ink.as_ref().map(|i| (i.arrival(0), i.arrival(9)));
        let (Some(Some(a)), Some(Some(z))) = (ink.map(|x| x.0), ink.map(|x| x.1)) else { panic!("arrivals") };
        assert!(z > a, "the burst's last character arrives after its first");
    }

    #[test]
    fn off_tracks_nothing() {
        let mut m = Motion::new(false);
        m.wrote(0, "text");
        assert!(m.ink.is_none() && !m.drying());
    }
}
