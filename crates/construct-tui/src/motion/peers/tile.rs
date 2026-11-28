//! Peer tile motion: per-row luminous beam arrivals, state/model/question
//! reaction sweeps, revoke shakes, and live ambient indicators (orbital working
//! beacon + cyan shimmer, pending golden radar pulse, orange `?N` heartbeat).
//!
//! The design choice: every tile effect is foreground-and-glyph only, so the
//! tile's pill background (`theme::SELECTED_BG`) stays intact while text beams
//! or shimmers across it, and idle online rows stop ticking once settled.

use super::{FIRST_DT, PeerState, PeerTileRow};
use crate::motion::shaders::{beam_reveal, shake, sheen};
use crate::motion::{FLASH_ERR, FLASH_OK, INK, WHITE};
use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::collections::{HashMap, HashSet};
use tachyonfx::fx;
use tachyonfx::pattern::SweepPattern;
use tachyonfx::{Duration, Effect, EffectRenderer, Interpolation};

/// Orbital frames for a `working` peer's live indicator in the tile.
pub(super) const ORBIT: [&str; 4] = ["◐", "◓", "◑", "◒"];

#[derive(Clone, PartialEq, Eq)]
struct TileMemo {
    state: PeerState,
    questions: usize,
    model: Option<(String, bool)>,
}

#[derive(Default)]
pub(super) struct TileFx {
    seen: HashMap<String, TileMemo>,
    fx: Vec<(String, Effect)>,
    live: bool,
}

impl TileFx {
    pub(super) fn running(&self) -> bool {
        !self.fx.is_empty() || self.live
    }

    pub(super) fn draw(
        &mut self,
        buf: &mut Buffer,
        tile: Option<(Rect, Vec<PeerTileRow>)>,
        dt: Duration,
        t: f32,
    ) -> bool {
        let Some((area, rows)) = tile else {
            self.seen.clear();
            self.fx.clear();
            self.live = false;
            return false;
        };
        let area = area.intersection(buf.area);
        let mut woke = false;
        let mut spawned = HashSet::new();
        let mut live_ids = HashSet::with_capacity(rows.len());
        self.live = false;

        for row in &rows {
            live_ids.insert(row.id.clone());
            self.live |= matches!(row.state, PeerState::Working | PeerState::Pending) || row.questions > 0;
            let memo = TileMemo { state: row.state, questions: row.questions, model: row.model.clone() };
            let effect = match self.seen.get(&row.id) {
                None => Some(beam_reveal(500, state_color(row.state))),
                Some(prev) if prev.state != memo.state => Some(state_transition(prev.state, memo.state)),
                Some(prev) if prev.model != memo.model => Some(
                    fx::fade_from_fg(theme::ACCENT2, (520, Interpolation::QuadOut))
                        .with_pattern(SweepPattern::left_to_right(8)),
                ),
                Some(prev) if memo.questions > prev.questions => Some(
                    fx::fade_from_fg(theme::ORANGE, (460, Interpolation::QuadOut))
                        .with_pattern(SweepPattern::left_to_right(6)),
                ),
                _ => None,
            };
            if let Some(e) = effect {
                self.fx.retain(|(id, _)| id != &row.id);
                self.fx.push((row.id.clone(), e));
                spawned.insert(row.id.clone());
                woke = true;
            }
            self.seen.insert(row.id.clone(), memo);
        }
        self.seen.retain(|id, _| live_ids.contains(id));

        for (i, row) in rows.iter().enumerate() {
            let y = area.y + i as u16;
            if y >= area.bottom() {
                break;
            }
            let r = Rect::new(area.x, y, area.width, 1);
            ambient_tile_row(buf, r, row, t);
            if let Some((_, e)) = self.fx.iter_mut().find(|(id, _)| id == &row.id) {
                let step = if spawned.contains(&row.id) { dt.min(FIRST_DT) } else { dt };
                buf.render_effect(e, r, step);
            }
        }
        self.fx.retain(|(id, e)| live_ids.contains(id) && e.running());
        woke
    }
}

fn state_color(state: PeerState) -> Color {
    match state {
        PeerState::Expected => theme::DIM,
        PeerState::Pending => theme::YELLOW,
        PeerState::Online => theme::GREEN,
        PeerState::Working => theme::CYAN,
        PeerState::Away => theme::ORANGE,
        PeerState::Stale => theme::FAINT,
        PeerState::Revoked => theme::RED,
    }
}

fn state_transition(from: PeerState, to: PeerState) -> Effect {
    match to {
        PeerState::Revoked => fx::parallel(&[fx::fade_from_fg(FLASH_ERR, (600, Interpolation::QuadOut)), shake(380)]),
        PeerState::Online if matches!(from, PeerState::Pending | PeerState::Working) => fx::sequence(&[
            fx::fade_from_fg(FLASH_OK, (480, Interpolation::QuadOut)).with_pattern(SweepPattern::left_to_right(8)),
            sheen(450),
        ]),
        _ => fx::fade_from_fg(state_color(to), (420, Interpolation::QuadOut))
            .with_pattern(SweepPattern::left_to_right(6)),
    }
}

/// Live ambient touches on a single peer tile row (foreground-only, keeping `SELECTED_BG`).
fn ambient_tile_row(buf: &mut Buffer, r: Rect, row: &PeerTileRow, t: f32) {
    if r.width < 3 {
        return;
    }
    let mark_x = r.x + 1;
    match row.state {
        PeerState::Working => {
            let pulse = (t * 5.0).sin() * 0.5 + 0.5;
            let glyph = ORBIT[((t * 8.0) as usize) % ORBIT.len()];
            buf[(mark_x, r.y)].set_symbol(glyph).set_fg(theme::lerp(theme::CYAN, WHITE, pulse * 0.75));
            shimmer_row(buf, r, 3, t, 1.9, theme::CYAN);
        }
        PeerState::Pending => {
            let pulse = (t * 4.5).sin() * 0.5 + 0.5;
            buf[(mark_x, r.y)].set_fg(theme::lerp(theme::YELLOW, WHITE, pulse * 0.7));
            shimmer_row(buf, r, 3, t, 2.2, WHITE);
        }
        _ => {}
    }
    if row.questions > 0 {
        let pulse = (t * 5.5).sin() * 0.5 + 0.5;
        let hi = theme::lerp(theme::ORANGE, WHITE, pulse * 0.7);
        let mut on = false;
        for x in r.x..r.right() {
            let cell = &mut buf[(x, r.y)];
            if cell.symbol() == "?" {
                on = true;
            } else if on && !cell.symbol().chars().all(|c| c.is_ascii_digit()) {
                on = false;
            }
            if on {
                cell.set_fg(hi);
            }
        }
    }
}

/// A soft specular wave gliding left-to-right across non-empty glyphs of one row.
fn shimmer_row(buf: &mut Buffer, r: Rect, start_dx: u16, t: f32, period: f32, tint: Color) {
    let span = r.width as f32 + 10.0;
    let head = ((t % period) / period) * span - 5.0;
    for dx in start_dx..r.width {
        let d = (dx as f32 - head).abs();
        if d >= 3.5 {
            continue;
        }
        let cell = &mut buf[(r.x + dx, r.y)];
        if cell.symbol().trim().is_empty() {
            continue;
        }
        let k = (1.0 - d / 3.5).powi(2) * 0.65;
        let base = if matches!(cell.fg, Color::Rgb(..)) { cell.fg } else { INK };
        cell.set_fg(theme::lerp(base, tint, k));
    }
}
