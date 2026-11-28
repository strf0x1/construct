//! Peer overlay motion: the JOIN/PEER popup (cyan dual-arm border draw,
//! lavender beam reveal + SAS sheen, multi-line selection sweep, confirm-pop
//! dissolve ghost) and the `/peer <name>` attach view (cyan tune-in border &
//! beam, live working border comet, top-edge scroll fade, fresh-output sweep,
//! and tune-out ghost).
//!
//! The design choice: selected choices are found in the drawn buffer via
//! `theme::SELECTED_BG` across wrapped rows, and the attach border comet touches
//! only `─` cells so titles and hints are never disturbed.

use super::{FIRST_DT, PeerAttach, PeerDecision, PeerPopup, snapshot};
use crate::motion::WHITE;
use crate::motion::shaders::{beam_reveal, border_draw, ghost, scroll_fade, sheen};
use crate::theme;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::ops::Range;
use tachyonfx::fx;
use tachyonfx::pattern::SweepPattern;
use tachyonfx::{Duration, Effect, EffectRenderer, Interpolation};

struct SavedPopup {
    key: String,
    area: Rect,
    cells: Vec<Cell>,
    sel: Option<Range<u16>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PopupPart {
    Edge,
    Inner,
    Selected,
}

#[derive(Default)]
pub(super) struct PopupFx {
    saved: Option<SavedPopup>,
    fx: Vec<(PopupPart, Effect)>,
    ghost: Option<(Rect, Effect)>,
}

impl PopupFx {
    pub(super) fn running(&self) -> bool {
        !self.fx.is_empty() || self.ghost.is_some()
    }

    pub(super) fn draw(
        &mut self,
        buf: &mut Buffer,
        popup: Option<PeerPopup>,
        decision: Option<PeerDecision>,
        dt: Duration,
    ) -> bool {
        let mut woke = false;
        if let Some(p) = popup {
            let area = p.area.intersection(buf.area);
            let inner = Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width.saturating_sub(4),
                height: area.height.saturating_sub(2),
            };
            let sel = selected_range(buf, inner);
            let mut spawned = false;
            match self.saved.as_ref() {
                Some(prev) if prev.key == p.key => {
                    if prev.sel != sel && sel.is_some() {
                        let sweep = fx::fade_from_fg(WHITE, (300, Interpolation::QuadOut))
                            .with_pattern(SweepPattern::left_to_right(6));
                        self.fx.retain(|(part, _)| *part != PopupPart::Selected);
                        self.fx.push((PopupPart::Selected, sweep));
                        woke = true;
                        spawned = true;
                    }
                }
                _ => {
                    self.ghost = None;
                    let reveal = if p.is_join {
                        fx::sequence(&[beam_reveal(400, theme::ACCENT2), fx::sleep(30), sheen(620)])
                    } else {
                        beam_reveal(400, theme::ACCENT2)
                    };
                    self.fx = vec![(PopupPart::Inner, reveal), (PopupPart::Edge, border_draw(400, theme::CYAN))];
                    woke = true;
                    spawned = true;
                }
            }
            self.saved = Some(SavedPopup { key: p.key, area, cells: snapshot(buf, area), sel: sel.clone() });
            let step = if spawned { dt.min(FIRST_DT) } else { dt };
            for (part, e) in &mut self.fx {
                let r = match part {
                    PopupPart::Edge => area,
                    PopupPart::Inner => inner,
                    PopupPart::Selected => match &sel {
                        Some(rng) => Rect { y: rng.start, height: rng.end - rng.start, ..inner },
                        None => continue,
                    },
                };
                if !r.is_empty() {
                    buf.render_effect(e, r, step);
                }
            }
            self.fx.retain(|(_, e)| e.running());
        } else {
            if let Some(prev) = self.saved.take() {
                self.fx.clear();
                let (tint, pop) = match decision {
                    Some(PeerDecision::Allow { pop }) => (theme::GREEN, pop),
                    Some(PeerDecision::Always { pop }) => (theme::YELLOW, pop),
                    Some(PeerDecision::Deny { pop }) => (theme::RED, pop),
                    Some(PeerDecision::Later) | None => (theme::DIM, false),
                };
                let rel_sel = pop
                    .then_some(prev.sel)
                    .flatten()
                    .map(|r| r.start.saturating_sub(prev.area.y)..r.end.saturating_sub(prev.area.y));
                self.ghost = Some((prev.area, ghost(prev.cells, rel_sel, tint, 420)));
                woke = true;
            }
            if let Some((area, e)) = self.ghost.as_mut() {
                buf.render_effect(e, area.intersection(buf.area), if woke { dt.min(FIRST_DT) } else { dt });
                if e.done() {
                    self.ghost = None;
                }
            }
        }
        woke
    }
}

struct SavedAttach {
    id: String,
    area: Rect,
    cells: Vec<Cell>,
    text_len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachPart {
    Edge,
    Inner,
    Fresh,
}

#[derive(Default)]
pub(super) struct AttachFx {
    saved: Option<SavedAttach>,
    fx: Vec<(AttachPart, Effect)>,
    comet: Option<Effect>,
    ghost: Option<(Rect, Effect)>,
    live: bool,
}

impl AttachFx {
    pub(super) fn running(&self) -> bool {
        !self.fx.is_empty() || self.ghost.is_some() || self.live
    }

    pub(super) fn draw(
        &mut self,
        buf: &mut Buffer,
        attach: Option<PeerAttach>,
        clean_snap: bool,
        dt: Duration,
    ) -> bool {
        let mut woke = false;
        if let Some(a) = attach {
            let area = a.area.intersection(buf.area);
            let inner = a.inner.intersection(buf.area);
            let mut spawned = false;
            match self.saved.as_ref() {
                Some(prev) if prev.id == a.id => {
                    if a.text_len > prev.text_len && prev.text_len > 0 {
                        let sweep = fx::fade_from_fg(WHITE, (380, Interpolation::QuadOut))
                            .with_pattern(SweepPattern::left_to_right(8));
                        self.fx.retain(|(p, _)| *p != AttachPart::Fresh);
                        self.fx.push((AttachPart::Fresh, sweep));
                        woke = true;
                        spawned = true;
                    }
                }
                _ => {
                    self.ghost = None;
                    self.fx = vec![
                        (AttachPart::Edge, border_draw(300, theme::CYAN)),
                        (AttachPart::Inner, beam_reveal(320, theme::CYAN)),
                    ];
                    woke = true;
                    spawned = true;
                }
            }
            if clean_snap {
                self.saved = Some(SavedAttach { id: a.id, area, cells: snapshot(buf, area), text_len: a.text_len });
            } else if let Some(prev) = self.saved.as_mut() {
                prev.text_len = a.text_len;
            }
            self.live = a.working;
            if a.working && self.comet.is_none() {
                self.comet = Some(border_comet(theme::CYAN, 1600));
                woke = true;
            } else if !a.working {
                self.comet = None;
            }
            if a.top > 0 {
                scroll_fade(buf, inner, a.top);
            }
            if let Some(e) = self.comet.as_mut() {
                buf.render_effect(e, area, dt);
            }
            let fresh_row = last_text_row(buf, inner);
            let step = if spawned { dt.min(FIRST_DT) } else { dt };
            for (part, e) in &mut self.fx {
                let r = match part {
                    AttachPart::Edge => area,
                    AttachPart::Inner => inner,
                    AttachPart::Fresh => match fresh_row {
                        Some(y) => Rect { y, height: 1, ..inner },
                        None => continue,
                    },
                };
                if !r.is_empty() {
                    buf.render_effect(e, r, step);
                }
            }
            self.fx.retain(|(_, e)| e.running());
        } else {
            self.live = false;
            self.comet = None;
            if let Some(prev) = self.saved.take() {
                self.fx.clear();
                self.ghost = Some((prev.area, ghost(prev.cells, None, theme::CYAN, 280)));
                woke = true;
            }
            if let Some((area, e)) = self.ghost.as_mut() {
                buf.render_effect(e, area.intersection(buf.area), if woke { dt.min(FIRST_DT) } else { dt });
                if e.done() {
                    self.ghost = None;
                }
            }
        }
        woke
    }
}

/// A cyan comet lapping the horizontal `─` segments of an attach box's top border.
fn border_comet(color: Color, period_ms: u32) -> Effect {
    fx::repeating(fx::effect_fn_buf((), (period_ms, Interpolation::Linear), move |_, ctx, buf| {
        let a = ctx.area;
        if a.width < 6 || a.height < 2 {
            return;
        }
        let tail = (a.width as f32 * 0.18).max(8.0);
        let head = ctx.timer.alpha() * (a.width as f32 + tail);
        for dx in 1..a.width - 1 {
            let d = head - dx as f32;
            if !(0.0..tail).contains(&d) {
                continue;
            }
            let cell = &mut buf[(a.x + dx, a.y)];
            if cell.symbol() != "─" {
                continue;
            }
            if d < 1.2 {
                cell.set_fg(WHITE).set_symbol("━");
            } else if d < 3.0 {
                cell.set_fg(theme::lerp(color, WHITE, (3.0 - d) / 2.0));
            } else {
                cell.set_fg(theme::lerp(color, WHITE, (1.0 - d / tail).powf(2.0) * 0.45));
            }
        }
    }))
}

/// Contiguous range of rows inside `inner` carrying `SELECTED_BG` (handles wrapped choices).
fn selected_range(buf: &Buffer, inner: Rect) -> Option<Range<u16>> {
    let mut start = None;
    let mut end = 0;
    for y in inner.y..inner.bottom() {
        if buf.cell((inner.x, y)).is_some_and(|c| c.bg == theme::SELECTED_BG) {
            start.get_or_insert(y);
            end = y + 1;
        } else if start.is_some() {
            break;
        }
    }
    start.map(|s| s..end)
}

fn last_text_row(buf: &Buffer, inner: Rect) -> Option<u16> {
    (inner.y..inner.bottom()).rev().find(|&y| (inner.x..inner.right()).any(|x| !buf[(x, y)].symbol().trim().is_empty()))
}
