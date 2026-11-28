//! Peer motion: lively animations for the peer tile (`tile.rs`), the JOIN/PEER
//! popup and `/peer <name>` attach view (`overlay.rs`), taking inspiration from
//! PR #397's luminous beam reveals, specular sheens, scroll fade, and
//! confirm-pop dissolve ghosts.
//!
//! The design choice: `peers/` knows nothing of motion. `view.rs` passes plain
//! geometry and state snapshots (`PeerFrame`) after drawing, and each layer
//! masks higher layers (`tile` over `attach`, `popup` over both) so transitions
//! and ghosts never bleed across overlays.

mod overlay;
mod tile;

use super::Motion;
use overlay::{AttachFx, PopupFx};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use tachyonfx::Duration;
use tile::TileFx;

/// Cap for the very first tick of a newly spawned effect so it never skips in.
const FIRST_DT: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PeerState {
    Expected,
    Pending,
    Online,
    Working,
    Away,
    Stale,
    Revoked,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PeerDecision {
    Allow { pop: bool },
    Always { pop: bool },
    Deny { pop: bool },
    Later,
}

pub(crate) struct PeerTileRow {
    pub(crate) id: String,
    pub(crate) state: PeerState,
    pub(crate) questions: usize,
    pub(crate) model: Option<(String, bool)>,
}

pub(crate) struct PeerPopup {
    pub(crate) area: Rect,
    pub(crate) key: String,
    pub(crate) is_join: bool,
}

pub(crate) struct PeerAttach {
    pub(crate) area: Rect,
    pub(crate) inner: Rect,
    pub(crate) id: String,
    pub(crate) top: usize,
    pub(crate) text_len: usize,
    pub(crate) working: bool,
}

#[derive(Default)]
pub(crate) struct PeerFrame {
    pub(crate) tile: Option<(Rect, Vec<PeerTileRow>)>,
    pub(crate) popup: Option<PeerPopup>,
    pub(crate) decision: Option<PeerDecision>,
    pub(crate) attach: Option<PeerAttach>,
}

#[derive(Default)]
pub(in crate::motion) struct PeerMotion {
    tile: TileFx,
    popup: PopupFx,
    attach: AttachFx,
}

impl PeerMotion {
    pub(in crate::motion) fn running(&self) -> bool {
        self.tile.running() || self.popup.running() || self.attach.running()
    }
}

impl Motion {
    /// Apply peer animations over the buffer after `peers::draw` ran.
    pub(crate) fn peers(&mut self, buf: &mut Buffer, frame: PeerFrame) {
        if !self.on {
            return;
        }
        let dt = self.frame;
        let t = self.started.elapsed().as_secs_f32();
        let tile_area = frame.tile.as_ref().map(|(r, _)| r.intersection(buf.area));
        let popup_area = frame.popup.as_ref().map(|p| p.area.intersection(buf.area));
        let mut woke = false;

        // Protect higher z-layers while lower layers animate or dissolve.
        let saved_popup = popup_area.map(|r| (r, snapshot(buf, r)));
        let saved_tile = tile_area.map(|r| (r, snapshot(buf, r)));
        woke |= self.peers.attach.draw(buf, frame.attach, frame.popup.is_none(), dt);
        if let Some((r, cells)) = saved_tile {
            restore(buf, r, &cells);
        }
        woke |= self.peers.tile.draw(buf, frame.tile, dt, t);
        if let Some((r, cells)) = saved_popup {
            restore(buf, r, &cells);
        }
        woke |= self.peers.popup.draw(buf, frame.popup, frame.decision, dt);

        if woke {
            self.wake();
        }
    }
}

fn snapshot(buf: &Buffer, area: Rect) -> Vec<Cell> {
    area.positions().map(|p| buf[p].clone()).collect()
}

fn restore(buf: &mut Buffer, area: Rect, cells: &[Cell]) {
    for (pos, c) in area.positions().zip(cells.iter()) {
        buf[pos] = c.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::INK;
    use crate::theme;
    use ratatui::style::Style;
    use ratatui::widgets::{Block, BorderType, Widget};
    use std::ops::Range;
    use std::time::Instant;

    fn step(m: &mut Motion, ms: u64) {
        m.last = Instant::now() - std::time::Duration::from_millis(ms);
        let mut scratch = Buffer::empty(Rect::new(0, 0, 1, 1));
        m.transcript(&mut scratch, &[], 0, 0, 0, false);
    }

    fn draw_tile(b: &mut Buffer, area: Rect, text: &str) {
        for p in area.positions() {
            b[p].set_symbol(" ").set_bg(theme::SELECTED_BG);
        }
        b.set_string(area.x, area.y, text, Style::new().fg(theme::GREEN).bg(theme::SELECTED_BG));
    }

    fn draw_popup(b: &mut Buffer, area: Rect, sel_rows: Range<u16>) {
        Block::bordered().border_type(BorderType::Rounded).render(area, b);
        b.set_string(area.x + 2, area.y + 1, "old-pc wants to join", Style::new().fg(theme::CYAN));
        for y in sel_rows {
            for x in area.x + 2..area.right() - 2 {
                b[(x, y)].set_bg(theme::SELECTED_BG);
            }
            b.set_string(area.x + 2, y, " ❯ Approve", Style::new().fg(theme::GREEN).bg(theme::SELECTED_BG));
        }
    }

    fn row(id: &str, state: PeerState) -> PeerTileRow {
        PeerTileRow { id: id.into(), state, questions: 0, model: None }
    }

    #[test]
    fn off_leaves_peer_surfaces_untouched() {
        let mut m = Motion::new(false);
        let area = Rect::new(0, 0, 30, 8);
        let mut b = Buffer::empty(area);
        let tile = Rect::new(10, 0, 20, 1);
        draw_tile(&mut b, tile, " ◉ old-pc working ");
        m.peers(
            &mut b,
            PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Working)])), ..Default::default() },
        );
        assert_eq!(b[(11, 0)].symbol(), "◉");
        assert!(!m.running());
    }

    #[test]
    fn tile_row_beams_in_and_working_peer_orbits_while_online_settles() {
        let mut m = Motion::new(true);
        let tile = Rect::new(0, 0, 24, 1);
        let mut b = Buffer::empty(tile);
        draw_tile(&mut b, tile, " ● old-pc online ");
        step(&mut m, 30);
        m.peers(&mut b, PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Online)])), ..Default::default() });
        assert_eq!(b[(15, 0)].symbol(), " ", "right edge not revealed yet");
        assert_eq!(b[(15, 0)].bg, theme::SELECTED_BG, "pill background preserved");
        for _ in 0..8 {
            draw_tile(&mut b, tile, " ● old-pc online ");
            step(&mut m, 100);
            m.peers(
                &mut b,
                PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Online)])), ..Default::default() },
            );
        }
        assert_eq!(b[(10, 0)].symbol(), "o");
        assert!(!m.running(), "quiet online peer stops ticking once revealed");

        draw_tile(&mut b, tile, " ◉ old-pc working ");
        step(&mut m, 30);
        m.peers(
            &mut b,
            PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Working)])), ..Default::default() },
        );
        assert!(tile::ORBIT.contains(&b[(1, 0)].symbol()), "working glyph orbits: {:?}", b[(1, 0)].symbol());
        assert!(m.running(), "working peer keeps live motion ticking");
    }

    #[test]
    fn tile_revoked_shakes_and_model_override_sweeps() {
        let mut m = Motion::new(true);
        let tile = Rect::new(0, 0, 26, 1);
        let mut b = Buffer::empty(tile);
        for _ in 0..8 {
            draw_tile(&mut b, tile, " ● old-pc online ");
            step(&mut m, 100);
            m.peers(
                &mut b,
                PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Online)])), ..Default::default() },
            );
        }
        assert!(!m.running());

        let with_model = PeerTileRow { model: Some(("small".into(), true)), ..row("old-pc", PeerState::Online) };
        draw_tile(&mut b, tile, " ● old-pc online small ");
        step(&mut m, 20);
        m.peers(&mut b, PeerFrame { tile: Some((tile, vec![with_model])), ..Default::default() });
        assert!(m.running(), "model change triggers lavender sweep");

        let mut moved = false;
        for _ in 0..10 {
            draw_tile(&mut b, tile, " ✕ old-pc revoked ");
            step(&mut m, 30);
            m.peers(
                &mut b,
                PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Revoked)])), ..Default::default() },
            );
            moved |= b[(1, 0)].symbol() != "✕";
        }
        assert!(moved, "revoked row shook at least once");
    }

    #[test]
    fn peer_popup_draws_border_sweeps_selection_and_dissolves_green() {
        let mut m = Motion::new(true);
        let area = Rect::new(0, 0, 34, 8);
        let mut b = Buffer::empty(area);
        draw_popup(&mut b, area, 3..5);
        step(&mut m, 30);
        m.peers(
            &mut b,
            PeerFrame {
                popup: Some(PeerPopup { area, key: "join\0old-pc".into(), is_join: true }),
                ..Default::default()
            },
        );
        assert_eq!(b[(17, 0)].symbol(), " ", "top middle of border arrives last");
        for _ in 0..14 {
            b = Buffer::empty(area);
            draw_popup(&mut b, area, 3..5);
            step(&mut m, 100);
            m.peers(
                &mut b,
                PeerFrame {
                    popup: Some(PeerPopup { area, key: "join\0old-pc".into(), is_join: true }),
                    ..Default::default()
                },
            );
        }
        assert_eq!(b[(17, 0)].symbol(), "─", "border settled whole");
        assert!(!m.peers.popup.running(), "arrival + sheen finished");

        b = Buffer::empty(area);
        draw_popup(&mut b, area, 5..6);
        step(&mut m, 16);
        m.peers(
            &mut b,
            PeerFrame {
                popup: Some(PeerPopup { area, key: "join\0old-pc".into(), is_join: true }),
                ..Default::default()
            },
        );
        assert!(m.peers.popup.running(), "moved selection sweeps");

        b = Buffer::empty(area);
        b.set_string(2, 3, "underlying", Style::new());
        step(&mut m, 16);
        m.peers(&mut b, PeerFrame { decision: Some(PeerDecision::Allow { pop: true }), ..Default::default() });
        assert!(m.peers.popup.running(), "ghost dissolving");
        for _ in 0..6 {
            b = Buffer::empty(area);
            b.set_string(2, 3, "underlying", Style::new());
            step(&mut m, 100);
            m.peers(&mut b, PeerFrame::default());
        }
        assert!(!m.peers.popup.running());
        assert_eq!(b[(2, 3)].symbol(), "u");
    }

    #[test]
    fn attach_scroll_fades_and_close_ghost_never_clobbers_tile() {
        let mut m = Motion::new(true);
        let area = Rect::new(0, 0, 40, 8);
        let inner = Rect::new(2, 1, 36, 6);
        let tile = Rect::new(24, 0, 16, 1);
        let draw_both = |b: &mut Buffer| {
            *b = Buffer::empty(area);
            Block::bordered().border_type(BorderType::Rounded).render(area, b);
            for y in inner.y..inner.bottom() {
                b.set_string(inner.x, y, "peer output", Style::new().fg(INK));
            }
            draw_tile(b, tile, " ● old-pc online");
        };
        let mut b = Buffer::empty(area);
        for _ in 0..6 {
            draw_both(&mut b);
            step(&mut m, 100);
            m.peers(
                &mut b,
                PeerFrame {
                    tile: Some((tile, vec![row("old-pc", PeerState::Online)])),
                    attach: Some(PeerAttach { area, inner, id: "old-pc".into(), top: 4, text_len: 20, working: false }),
                    ..Default::default()
                },
            );
        }
        assert_ne!(b[(inner.x, inner.y)].fg, INK, "top attach row faded when scrolled");
        assert_eq!(b[(inner.x, inner.y + 4)].fg, INK, "lower attach row stays bright");

        b = Buffer::empty(area);
        draw_tile(&mut b, tile, " ● old-pc online");
        step(&mut m, 16);
        m.peers(&mut b, PeerFrame { tile: Some((tile, vec![row("old-pc", PeerState::Online)])), ..Default::default() });
        assert!(m.peers.attach.running(), "close ghost running");
        assert_eq!(b[(tile.x + 1, tile.y)].symbol(), "●", "tile stayed crisp over attach ghost");
    }
}
