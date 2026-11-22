//! The permission panel's motion: its border draws itself and its contents
//! fade in as a new question arrives, a moved selection is swept once, and
//! its cells are kept each frame so that, once answered, `Motion::rule` can
//! dissolve it as a ghost. The selected row is found in the drawn frame by
//! its background (`theme::SELECTED_BG`), so `prompt.rs` knows nothing of
//! motion.

use super::shaders::border_draw;
use super::{Motion, WHITE};
use crate::theme;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use tachyonfx::fx;
use tachyonfx::pattern::SweepPattern;
use tachyonfx::{EffectRenderer, Interpolation};

/// The panel as drawn: where, its cells (row by row), and the row of its
/// selected choice.
pub(super) struct Panel {
    pub(super) area: Rect,
    pub(super) cells: Vec<Cell>,
    sel: Option<u16>,
}

/// Which part of the panel a panel effect covers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Part {
    Edge,
    Inner,
    Selected,
}

impl Motion {
    /// The question on screen was answered: the next ghost washes green when
    /// it was let through, amber when not, and the next panel is a new one.
    pub(crate) fn answered(&mut self, allowed: bool) {
        self.tint = if allowed { theme::GREEN } else { theme::YELLOW };
        self.rearm = true;
    }

    /// The permission panel was just drawn over `area`. A new question draws
    /// its border and fades its contents in (inside the 450 ms the panel
    /// ignores keys); a moved selection gets one sweep; and the panel's cells
    /// are kept, before any effect, for the ghost once it is gone.
    pub(crate) fn panel(&mut self, buf: &mut Buffer, area: Rect) {
        if !self.on {
            return;
        }
        let area = area.intersection(buf.area);
        let inner = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(4),
            height: area.height.saturating_sub(2),
        };
        let sel = (inner.y..inner.y + inner.height)
            .find(|y| buf.cell((inner.x, *y)).is_some_and(|c| c.bg == theme::SELECTED_BG));
        match self.panel.as_ref() {
            Some(_) if !self.rearm => {
                if self.panel.as_ref().and_then(|p| p.sel) != sel && sel.is_some() {
                    let sweep = fx::fade_from_fg(WHITE, (320, Interpolation::QuadOut))
                        .with_pattern(SweepPattern::left_to_right(6));
                    self.panel_fx.retain(|(p, _)| *p != Part::Selected);
                    self.panel_fx.push((Part::Selected, sweep));
                }
            }
            _ => {
                self.wake();
                self.rearm = false;
                self.ghost = None;
                self.panel_fx = vec![
                    (
                        Part::Inner,
                        fx::coalesce((420, Interpolation::QuadOut)).with_pattern(SweepPattern::left_to_right(10)),
                    ),
                    (Part::Edge, border_draw(420)),
                ];
            }
        }
        let cells = area.positions().map(|p| buf[p].clone()).collect();
        self.panel = Some(Panel { area, cells, sel });
        let dt = self.frame;
        for (part, e) in &mut self.panel_fx {
            let r = match part {
                Part::Edge => area,
                Part::Inner => inner,
                Part::Selected => match sel {
                    Some(y) => Rect { y, height: 1, ..inner },
                    None => continue,
                },
            };
            buf.render_effect(e, r, dt);
        }
        self.panel_fx.retain(|(_, e)| e.running());
    }
}
