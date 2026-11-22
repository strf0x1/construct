//! The splash's intro, queued once at startup: the title resolves out of
//! shaded blocks from its middle outward and a white glow sweeps it once, then the tagline, the session
//! rows and the key hints fade in after it, each a beat later. The splash
//! records where its parts landed in `App::rows` (title, tagline, rows,
//! hints), and these effects are drawn only while the splash is what is on
//! screen; the first real block (a turn, a resumed session) drops them. They
//! change cells after the first frame is drawn, so the first frame waits on
//! nothing.

#[cfg(test)]
use super::Place;
use super::{Motion, Target, WHITE};
use tachyonfx::fx::{self, EvolveSymbolSet};
use tachyonfx::pattern::{RadialPattern, SweepPattern};
use tachyonfx::{CellFilter, Interpolation};

/// The splash's parts, in the order `App::splash` records them.
pub(crate) const TITLE: usize = 0;
pub(crate) const TAGLINE: usize = 1;
pub(crate) const ROWS: usize = 2;
pub(crate) const HINTS: usize = 3;

impl Motion {
    /// Queue the intro. Only `from_env` calls this, so a bare `Motion::new`
    /// (tests, `take`) starts with nothing moving.
    pub(super) fn splash_intro(&mut self) {
        // The title resolves out of shaded blocks from its middle outward,
        // then a white glow sweeps it once. Its rect ends at the logo's
        // last column (`Place::width`), so the sweep spends its time on it.
        let resolve = fx::evolve_into(EvolveSymbolSet::Shaded, (1100, Interpolation::SineOut))
            .with_pattern(RadialPattern::with_transition((0.5, 0.5), 8.0))
            .with_filter(CellFilter::NonEmpty);
        let glow = fx::fade_from_fg(WHITE, (800, Interpolation::QuadOut)).with_pattern(SweepPattern::left_to_right(10));
        let fade = |delay: u32, ms: u32| {
            let reveal = fx::coalesce((ms, Interpolation::QuadOut)).with_pattern(SweepPattern::right_to_left(8));
            fx::prolong_start(delay, reveal)
        };
        self.fx = vec![
            (Target::Splash(TITLE), fx::sequence(&[resolve, fx::sleep(100), glow])),
            (Target::Splash(TAGLINE), fade(600, 700)),
            (Target::Splash(ROWS), fade(850, 600)),
            (Target::Splash(HINTS), fade(1100, 500)),
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

    /// A splash: title at row 1, tagline 2, rows 4..7, hints 8.
    fn draw(b: &mut Buffer) -> Vec<Place> {
        *b = Buffer::empty(Rect::new(0, 0, 40, 10));
        b.set_string(2, 1, "◆ construct", Style::new());
        b.set_string(2, 2, "lean · fast", Style::new());
        for y in 4..7 {
            b.set_string(2, y, "model    x", Style::new());
        }
        b.set_string(2, 8, "enter send", Style::new());
        [(1, 1), (2, 1), (4, 3), (8, 1)].map(|(start, len)| Place { start, len, width: 12 }).to_vec()
    }

    fn frame(m: &mut Motion, b: &mut Buffer, ms: u64, splash: bool) {
        let rows = draw(b);
        m.last = Instant::now() - std::time::Duration::from_millis(ms);
        m.transcript(b, &rows, 0, 0, 10, splash);
    }

    #[test]
    fn the_title_resolves_out_of_blocks_and_the_rest_follows() {
        let mut m = Motion::new(true);
        m.splash_intro();
        let mut b = Buffer::empty(Rect::new(0, 0, 1, 1));
        let mut blocks = false;
        for _ in 0..12 {
            frame(&mut m, &mut b, 50, true);
            let title: String = (2..13).map(|x| b[(x, 1)].symbol().to_string()).collect();
            blocks |= title.chars().any(|c| "░▒▓█".contains(c));
            assert_eq!(b[(30, 1)].symbol(), " ", "the empty row beside it stays empty");
        }
        assert!(blocks, "the title passes through shaded blocks in its first 600 ms");
        assert_eq!(b[(2, 8)].symbol(), " ", "the hints are still waiting their turn");
        for _ in 0..25 {
            frame(&mut m, &mut b, 100, true);
        }
        assert!(!m.running(), "the intro runs out");
        assert_eq!(b[(2, 1)].symbol(), "◆");
        assert_eq!(b[(2, 8)].symbol(), "e");
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
