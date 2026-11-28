//! The peer tile: one row per peer in the upper right, with the state a
//! human needs at a glance (`pending`, `online`, `working`, `away`, `stale`,
//! `revoked`) and the model its calls use, marked `(hub)` when the hub set it.
//!
//! The design choice: the state is derived here from the hub's plain facts
//! (registry standing, live link, turn, last seen), so the hub reports what
//! it knows and the words a person reads live in one pure function.

use super::types::{PeerRow, Standing};
use crate::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

/// A pinned peer offline for longer than this is `stale`, not `away`.
const AWAY_SECS: u64 = 15 * 60;

/// What the tile says about a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tile {
    Expected,
    Pending,
    Online,
    Working,
    Away,
    Stale,
    Revoked,
}

impl Tile {
    pub(super) fn of(row: &PeerRow, now: u64) -> Self {
        match row.standing {
            Standing::Revoked => Self::Revoked,
            Standing::Pending => Self::Pending,
            Standing::Expected => Self::Expected,
            Standing::Pinned if row.online && row.working => Self::Working,
            Standing::Pinned if row.online => Self::Online,
            Standing::Pinned => match row.last_seen {
                Some(t) if now.saturating_sub(t) < AWAY_SECS => Self::Away,
                _ => Self::Stale,
            },
        }
    }

    pub(super) fn word(self) -> &'static str {
        match self {
            Self::Expected => "expected",
            Self::Pending => "pending",
            Self::Online => "online",
            Self::Working => "working",
            Self::Away => "away",
            Self::Stale => "stale",
            Self::Revoked => "revoked",
        }
    }

    fn look(self) -> (&'static str, Color) {
        match self {
            Self::Expected => ("○", theme::DIM),
            Self::Pending => ("◆", theme::YELLOW),
            Self::Online => ("●", theme::GREEN),
            Self::Working => ("◉", theme::CYAN),
            Self::Away => ("◌", theme::ORANGE),
            Self::Stale => ("◌", theme::FAINT),
            Self::Revoked => ("✕", theme::RED),
        }
    }
}

/// The tile's rows; empty when there are no peers (or no hub).
pub(super) fn lines(rows: &[PeerRow], now: u64) -> Vec<Line<'static>> {
    rows.iter()
        .map(|r| {
            let t = Tile::of(r, now);
            let (mark, color) = t.look();
            let mut spans = vec![
                Span::styled(format!(" {mark} "), Style::new().fg(color)),
                Span::styled(r.id.clone(), Style::new().add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {}", t.word()), Style::new().fg(color)),
            ];
            if let Some(code) = r.code.as_deref().filter(|_| t == Tile::Pending) {
                spans.push(Span::styled(
                    format!(" {code}"),
                    Style::new().fg(theme::YELLOW).add_modifier(Modifier::BOLD),
                ));
            }
            if !r.questions.is_empty() {
                spans.push(Span::styled(format!(" ?{}", r.questions.len()), Style::new().fg(theme::ORANGE)));
            }
            if let Some((model, set)) = &r.model {
                // The model's own name, without its provider path.
                let short = model.rsplit('/').next().unwrap_or(model);
                spans.push(if *set {
                    Span::styled(format!(" {short} (hub)"), Style::new().fg(theme::ACCENT2))
                } else {
                    Span::styled(format!(" {short}"), Style::new().fg(theme::DIM))
                });
            }
            spans.push(Span::raw(" "));
            Line::from(spans)
        })
        .collect()
}

/// One visible row of the peer tile, reported for `motion`.
pub(crate) struct TileRow {
    pub(crate) id: String,
    pub(crate) state: Tile,
    pub(crate) questions: usize,
    pub(crate) model: Option<(String, bool)>,
}

/// Draw the peer tile in the upper right of `area`, returning its rect and rows.
pub(super) fn render(f: &mut Frame, area: Rect, rows: &[PeerRow], now: u64) -> Option<(Rect, Vec<TileRow>)> {
    let tile_lines = lines(rows, now);
    if tile_lines.is_empty() {
        return None;
    }
    let w = tile_lines.iter().map(Line::width).max().unwrap_or(0).min(area.width as usize) as u16;
    let h = (tile_lines.len() as u16).min(area.height / 2);
    let at = Rect::new(area.x + area.width - w, area.y, w, h);
    f.render_widget(Clear, at);
    f.render_widget(Paragraph::new(tile_lines).style(Style::new().bg(theme::SELECTED_BG)), at);
    let visible = rows
        .iter()
        .take(h as usize)
        .map(|r| TileRow {
            id: r.id.clone(),
            state: Tile::of(r, now),
            questions: r.questions.len(),
            model: r.model.clone(),
        })
        .collect();
    Some((at, visible))
}

#[cfg(test)]
fn text(l: &Line) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::super::types::{Question, test_row as row};
    use super::*;

    #[test]
    fn states_follow_the_registry_the_link_and_the_clock() {
        let pinned =
            |online, working, seen| PeerRow { online, working, last_seen: seen, ..row("pi", Standing::Pinned) };
        let at = 10_000;
        assert_eq!(Tile::of(&pinned(true, true, None), at), Tile::Working);
        assert_eq!(Tile::of(&pinned(true, false, None), at), Tile::Online);
        assert_eq!(Tile::of(&pinned(false, false, Some(at - 60)), at), Tile::Away);
        assert_eq!(Tile::of(&pinned(false, false, Some(at - AWAY_SECS)), at), Tile::Stale);
        assert_eq!(Tile::of(&pinned(false, false, None), at), Tile::Stale);
        assert_eq!(Tile::of(&row("x", Standing::Revoked), at), Tile::Revoked);
        assert_eq!(Tile::of(&row("x", Standing::Pending), at), Tile::Pending);
    }

    #[test]
    fn a_pending_row_shows_its_code_and_open_questions_are_counted() {
        let join = PeerRow { code: Some("K7M-4QP".into()), ..row("old-pc", Standing::Pending) };
        let mut busy = PeerRow { working: true, ..row("pi", Standing::Pinned) };
        busy.questions = vec![Question { title: "t".into(), options: vec![] }];
        let l = lines(&[join, busy], 0);
        assert_eq!(text(&l[0]), "◆ old-pc pending K7M-4QP");
        assert_eq!(text(&l[1]), "◉ pi working ?1");
    }

    #[test]
    fn the_model_shows_without_its_provider_and_an_override_is_marked() {
        let own = PeerRow { model: Some(("openrouter/vendor/big".into(), false)), ..row("pi", Standing::Pinned) };
        let set = PeerRow { model: Some(("small".into(), true)), ..row("pc", Standing::Pinned) };
        let l = lines(&[own, set], 0);
        assert_eq!(text(&l[0]), "● pi online big");
        assert_eq!(text(&l[1]), "● pc online small (hub)");
        assert_eq!(l[1].spans.iter().rev().nth(1).unwrap().style.fg, Some(theme::ACCENT2));
    }
}
