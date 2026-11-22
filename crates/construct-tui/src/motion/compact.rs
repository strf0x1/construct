//! Compaction's motion. construct keeps the whole transcript on screen when it
//! compacts (only what the model is sent shrinks), so nothing is dissolved
//! away: a lavender wave runs down the transcript above the `◇ compacted`
//! notice and ends at it, as if folding into it, and the notice resolves out
//! of block glyphs. When the next usage shows the context share dropped, the
//! ctx gauge drains down to it instead of jumping.

use super::{Motion, Target};
use crate::theme;
use std::time::{Duration, Instant};
use tachyonfx::Interpolation;
use tachyonfx::fx::{self, EvolveSymbolSet};
use tachyonfx::pattern::SweepPattern;

/// How long the gauge takes to drain.
pub(super) const DRAIN: Duration = Duration::from_millis(900);
/// A drop smaller than this (points of the window) just shows the new value.
const DRAIN_MIN: u64 = 10;

impl Motion {
    /// Block `i` is the notice that history was compacted.
    pub(crate) fn compacted(&mut self, i: usize) {
        if !self.on {
            return;
        }
        self.wake();
        let wave =
            fx::fade_from_fg(theme::ACCENT2, (900, Interpolation::SineInOut)).with_pattern(SweepPattern::down_to_up(6)); // runs reversed: the top clears first
        self.fx.push((Target::Above(i), wave));
        // blocks while the wave comes down, then the notice resolves left to right
        let resolve = fx::evolve_into(EvolveSymbolSet::Quadrants, (800, Interpolation::SineOut))
            .with_pattern(SweepPattern::left_to_right(10));
        self.fx.push((Target::Block(i), fx::prolong_start(500, resolve)));
    }

    /// The context share went from `old` to `new` percent. A real drop (a
    /// compaction's) drains over `DRAIN`; anything else shows at once.
    pub(crate) fn gauge(&mut self, old: u64, new: u64) {
        if !self.on {
            return;
        }
        self.gauge = (old >= new + DRAIN_MIN).then(|| (self.gauge_shown(old), new, Instant::now()));
    }

    /// The share the status line shows now: `pct`, or on its way down to it.
    pub(crate) fn gauge_shown(&self, pct: u64) -> u64 {
        match self.gauge {
            Some((from, to, since)) if to == pct && since.elapsed() < DRAIN => {
                let t = since.elapsed().as_secs_f32() / DRAIN.as_secs_f32();
                let k = 1.0 - (1.0 - t).powi(3);
                (from as f32 - (from - to) as f32 * k).round() as u64
            }
            _ => pct,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_drains_and_a_rise_does_not() {
        let mut m = Motion::new(true);
        m.gauge(87, 14);
        let shown = m.gauge_shown(14);
        assert!(shown > 70, "starts near the old share: {shown}");
        assert!(m.running());
        m.gauge = m.gauge.map(|(f, t, _)| (f, t, Instant::now() - DRAIN));
        assert_eq!(m.gauge_shown(14), 14, "lands on the new share");
        assert!(!m.running());
        m.gauge(14, 40);
        assert_eq!(m.gauge_shown(40), 40, "a rise shows at once");
        m.gauge(40, 35);
        assert_eq!(m.gauge_shown(35), 35, "a small drop shows at once");
    }

    #[test]
    fn off_never_drains() {
        let mut m = Motion::new(false);
        m.gauge(87, 14);
        assert_eq!(m.gauge_shown(14), 14);
        m.compacted(3);
        assert!(!m.running());
    }

    #[test]
    fn a_compaction_waves_above_and_resolves_the_notice() {
        let mut m = Motion::new(true);
        m.compacted(2);
        let targets: Vec<Target> = m.fx.iter().map(|(t, _)| *t).collect();
        assert_eq!(targets, [Target::Above(2), Target::Block(2)]);
    }
}
