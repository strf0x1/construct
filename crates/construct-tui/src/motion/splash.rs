//! The splash's intro, queued once at startup: a luminous beam traces the
//! heavy geometric wordmark left to right, the tagline and a thin divider rule
//! draw in underneath, the session rows and key hints cascade in cleanly, and
//! a soft specular sheen glides across the finished logo. The splash records
//! where its parts landed in `App::rows` (title, tagline, rule, rows, hints),
//! and these effects are drawn only while the splash is what is on screen; the
//! first real block (a turn, a resumed session) drops them. They change cells
//! after the first frame is drawn, so the first frame waits on nothing.

#[cfg(test)]
use super::Place;
use super::shaders::{beam_reveal, rule_draw, sheen};
use super::{Motion, Target};
use crate::theme;
use tachyonfx::fx;

/// The splash's parts, in the order `App::splash` records them.
pub(crate) const TITLE: usize = 0;
pub(crate) const TAGLINE: usize = 1;
pub(crate) const RULE: usize = 2;
pub(crate) const ROWS: usize = 3;
pub(crate) const HINTS: usize = 4;

impl Motion {
    /// Queue the intro. Only `from_env` calls this, so a bare `Motion::new`
    /// (tests, `take`) starts with nothing moving.
    pub(super) fn splash_intro(&mut self) {
        let trace = beam_reveal(850, theme::CYAN);
        let gloss = sheen(680);
        let cascade = |delay: u32, ms: u32, edge| fx::prolong_start(delay, beam_reveal(ms, edge));
        self.fx = vec![
            (Target::Splash(TITLE), fx::sequence(&[trace, fx::sleep(80), gloss])),
            (Target::Splash(TAGLINE), cascade(320, 550, theme::ACCENT2)),
            (Target::Splash(RULE), fx::prolong_start(420, rule_draw(520))),
            (Target::Splash(ROWS), cascade(560, 550, theme::ACCENT)),
            (Target::Splash(HINTS), cascade(740, 520, theme::ACCENT)),
        ];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;
    use std::time::Instant;

    /// A centered splash: title at row 1, tagline 3, rule 4, rows 6..9, hints 10.
    fn draw(b: &mut Buffer) -> Vec<Place> {
        *b = Buffer::empty(Rect::new(0, 0, 50, 12));
        b.set_string(6, 1, "┏━━ ┏━┓ ┏┓┃", Style::new().fg(theme::ACCENT));
        b.set_string(6, 3, "◆ lean · fast", Style::new().fg(theme::SOFT));
        b.set_string(6, 4, "───────────", Style::new().fg(theme::FAINT));
        for y in 6..9 {
            b.set_string(6, y, "model    x", Style::new().fg(theme::DIM));
        }
        b.set_string(6, 10, "enter send", Style::new().fg(theme::ACCENT));
        [(6, 1, 1, 11), (6, 3, 1, 13), (6, 4, 1, 11), (6, 6, 3, 10), (6, 10, 1, 10)]
            .map(|(left, start, len, width)| Place { left, start, len, width })
            .to_vec()
    }

    fn frame(m: &mut Motion, b: &mut Buffer, ms: u64, splash: bool) {
        let rows = draw(b);
        m.last = Instant::now() - std::time::Duration::from_millis(ms);
        m.transcript(b, &rows, 0, 0, 12, splash);
    }

    #[test]
    fn the_beam_traces_the_logo_without_shaded_blocks_and_the_rest_follows() {
        let mut m = Motion::new(true);
        m.splash_intro();
        let mut b = Buffer::empty(Rect::new(0, 0, 1, 1));
        let mut partial = false;
        let mut bright_edge = false;
        for _ in 0..10 {
            frame(&mut m, &mut b, 40, true);
            let title: String = (6..17).map(|x| b[(x, 1)].symbol().to_string()).collect();
            assert!(!title.chars().any(|c| "░▒▓█".contains(c)), "no shaded blocks: {title:?}");
            partial |= b[(6, 1)].symbol() == "┏" && b[(16, 1)].symbol() == " ";
            bright_edge |= (6..17).any(|x| b[(x, 1)].fg == super::super::WHITE);
        }
        assert!(partial, "the beam reveals the logo left to right");
        assert!(bright_edge, "the beam carries a luminous leading edge");
        assert_eq!(b[(6, 10)].symbol(), " ", "the hints are still waiting their turn");
        for _ in 0..25 {
            frame(&mut m, &mut b, 100, true);
        }
        assert!(!m.running(), "the intro runs out");
        assert_eq!(b[(6, 1)].symbol(), "┏");
        assert_eq!(b[(6, 4)].symbol(), "─");
        assert_eq!(b[(6, 10)].symbol(), "e");
    }

    #[test]
    fn a_real_block_drops_the_intro() {
        let mut m = Motion::new(true);
        m.splash_intro();
        let mut b = Buffer::empty(Rect::new(0, 0, 1, 1));
        frame(&mut m, &mut b, 20, false);
        assert!(m.fx.is_empty(), "nothing left to draw over a transcript");
    }
}
