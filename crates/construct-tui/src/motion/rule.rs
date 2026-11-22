//! The rule above the input: a comet laps it while a turn runs, in the
//! phase's color (shared with the activity line through `hue`), and docks
//! with one flash when the turn ends. It is drawn last in a frame, so an
//! answered permission panel's ghost is drawn here too, over everything.

use super::Motion;
use super::shaders::{comet, ghost};
use crate::theme;
use crate::turn::Phase;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tachyonfx::fx;
use tachyonfx::{EffectRenderer, Interpolation};

impl Motion {
    /// The rule's color: the old pulse while a turn runs with motion off, the
    /// comet's faint track otherwise.
    pub(crate) fn rule_color(&self, busy: bool, phase: f32) -> Color {
        if busy && !self.on { theme::pulse(phase, theme::FAINT, theme::ACCENT) } else { theme::FAINT }
    }

    /// The turn's phase this frame (`None` when idle): swap the comet to the
    /// phase's color, or dock it with one flash when the turn ends.
    pub(crate) fn turn(&mut self, phase: Option<Option<&Phase>>) {
        if !self.on {
            return;
        }
        self.writing = matches!(phase, Some(Some(Phase::Writing)));
        match phase {
            Some(p) => {
                let (color, period) = hue(p);
                if self.comet.as_ref().map(|c| c.0) != Some(color) {
                    self.wake();
                    self.comet = Some((color, comet(color, period)));
                }
            }
            None => {
                if self.comet.take().is_some() {
                    self.dock = Some(fx::fade_from_fg(theme::ACCENT, (900, Interpolation::QuadOut)));
                }
            }
        }
    }

    /// Draw the comet, or the dock flash, over the rule, and a panel that
    /// was up last frame as a ghost dissolving over what replaced it. Called
    /// last in a frame without a panel, so the ghost lies over everything.
    pub(crate) fn rule(&mut self, buf: &mut Buffer, rect: Rect) {
        let rect = rect.intersection(buf.area);
        let dt = self.frame;
        if let Some(p) = self.panel.take() {
            self.panel_fx.clear();
            let tint = std::mem::replace(&mut self.tint, theme::DIM);
            self.rearm = false;
            self.ghost = Some((p.area, ghost(p.cells, tint, 420)));
        }
        if let Some((area, e)) = self.ghost.as_mut() {
            buf.render_effect(e, area.intersection(buf.area), dt);
            if e.done() {
                self.ghost = None;
            }
        }
        if let Some((_, e)) = self.comet.as_mut() {
            buf.render_effect(e, rect, dt);
        }
        if let Some(e) = self.dock.as_mut() {
            buf.render_effect(e, rect, dt);
            if e.done() {
                self.dock = None;
            }
        }
    }
}

/// The comet's color and lap time for a turn's phase; also the activity
/// line's highlight, so the two always agree.
pub(crate) fn hue(phase: Option<&Phase>) -> (Color, u32) {
    match phase {
        Some(Phase::Routing) => (theme::CYAN, 1600),
        Some(Phase::Waiting) => (theme::ACCENT, 1900),
        Some(Phase::Thinking) => (theme::ACCENT2, 1900),
        Some(Phase::Writing) => (theme::ACCENT, 1600),
        Some(Phase::Tool(_)) => (theme::YELLOW, 1200),
        None => (theme::DIM, 1900),
    }
}
