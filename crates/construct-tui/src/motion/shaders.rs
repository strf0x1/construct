//! The hand-written effects `Motion` schedules, each an `effect_fn_buf` over a
//! rect (plus the stateless viewport top-edge fade). They change only glyphs
//! and foreground colors of cells already drawn (the ghost puts back cells the
//! panel itself drew), never a background the panel did not set, so they read
//! the same on any terminal theme.

use super::{FLASH_ERR, FLASH_OK, GUTTER_X, INK, WHITE};
use crate::theme;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::ops::Range;
use tachyonfx::fx;
use tachyonfx::{Effect, Interpolation};

/// Darker than `FAINT` (`#464c66`) for text receding past the top edge.
const SCROLL_DEPTH: Color = Color::Rgb(38, 42, 60);
/// Confirm pop highlight for a denied choice.
const FLASH_DENY: Color = Color::Rgb(255, 232, 175);

/// A bright head with a fading tail, lapping the rule left to right.
pub(super) fn comet(color: Color, period_ms: u32) -> Effect {
    fx::repeating(fx::effect_fn_buf((), (period_ms, Interpolation::Linear), move |_, ctx, buf| {
        let a = ctx.area;
        let tail = (a.width as f32 * 0.22).max(10.0);
        let head = ctx.timer.alpha() * (a.width as f32 + tail);
        for x in 0..a.width {
            let d = head - x as f32;
            if !(0.0..tail).contains(&d) {
                continue;
            }
            let cell = &mut buf[(a.x + x, a.y)];
            if d < 1.0 {
                cell.set_fg(WHITE).set_symbol("━");
            } else if d < 3.0 {
                cell.set_fg(theme::lerp(color, WHITE, (3.0 - d) / 2.0));
            } else {
                cell.set_fg(theme::lerp(theme::FAINT, color, (1.0 - d / tail).powf(1.6)));
            }
        }
    }))
}

/// Luminous left-to-right beam reveal: strokes ahead of the beam stay blank,
/// the leading edge lights bright white and cools through `edge` into each
/// cell's own color. No shaded block noise.
pub(super) fn beam_reveal(ms: u32, edge: Color) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::CubicOut), move |_, ctx, buf| {
        let a = ctx.area;
        let alpha = ctx.timer.alpha();
        let trail = 6.0f32;
        let head = alpha * (a.width as f32 + trail);
        for dy in 0..a.height {
            for dx in 0..a.width {
                let d = head - dx as f32;
                let cell = &mut buf[(a.x + dx, a.y + dy)];
                if alpha == 0.0 || d <= 0.0 {
                    cell.set_symbol(" ");
                } else if d < trail && alpha < 1.0 && !cell.symbol().trim().is_empty() {
                    if d < 1.3 {
                        cell.set_fg(WHITE);
                    } else {
                        let k = ((d - 1.3) / (trail - 1.3)).clamp(0.0, 1.0);
                        let target = if matches!(cell.fg, Color::Rgb(..)) { cell.fg } else { INK };
                        let mid = theme::lerp(WHITE, edge, (k * 2.0).min(1.0));
                        cell.set_fg(theme::lerp(mid, target, k));
                    }
                }
            }
        }
    })
}

/// A soft diagonal specular sheen gliding across non-empty glyphs once.
pub(super) fn sheen(ms: u32) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::SineInOut), |_, ctx, buf| {
        let a = ctx.area;
        let band = 7.0f32;
        let span = a.width as f32 + a.height as f32 * 2.0 + band * 2.0;
        let pos = ctx.timer.alpha() * span - band;
        for dy in 0..a.height {
            for dx in 0..a.width {
                let cell = &mut buf[(a.x + dx, a.y + dy)];
                if cell.symbol().trim().is_empty() {
                    continue;
                }
                let dist = ((dx as f32 + dy as f32 * 2.0) - pos).abs();
                if dist < band {
                    let k = (1.0 - dist / band).powi(2) * 0.72;
                    let base = if matches!(cell.fg, Color::Rgb(..)) { cell.fg } else { INK };
                    cell.set_fg(theme::lerp(base, WHITE, k));
                }
            }
        }
    })
}

/// Draws a thin horizontal rule left-to-right with a bright `━` tip.
pub(super) fn rule_draw(ms: u32) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::CubicOut), |_, ctx, buf| {
        let a = ctx.area;
        let alpha = ctx.timer.alpha();
        let trail = 5.0f32;
        let head = alpha * (a.width as f32 + trail);
        for dx in 0..a.width {
            let d = head - dx as f32;
            let cell = &mut buf[(a.x + dx, a.y)];
            if alpha == 0.0 || d <= 0.0 {
                cell.set_symbol(" ");
            } else if d < 1.2 && alpha < 1.0 {
                cell.set_symbol("━").set_fg(WHITE);
            } else if d < trail && alpha < 1.0 {
                let k = ((d - 1.2) / (trail - 1.2)).clamp(0.0, 1.0);
                cell.set_fg(theme::lerp(theme::ACCENT, theme::FAINT, k));
            }
        }
    })
}

/// Attenuate the top rows of a scrolled viewport (`top > 0`) so text softly
/// fades darker as it scrolls up past the top edge. Foreground-only.
pub(super) fn scroll_fade(buf: &mut Buffer, rect: Rect, top: usize) {
    let rect = rect.intersection(buf.area);
    if top == 0 || rect.height < 4 || rect.width == 0 {
        return;
    }
    let strength = (top.min(3) as f32) / 3.0;
    const WEIGHTS: [f32; 3] = [0.74, 0.46, 0.20];
    for (dy, &w) in WEIGHTS.iter().enumerate() {
        let y = rect.y + dy as u16;
        if y >= rect.bottom() {
            break;
        }
        let k = w * strength;
        for x in rect.x..rect.right() {
            let cell = &mut buf[(x, y)];
            if cell.symbol().trim().is_empty() {
                continue;
            }
            let base = if matches!(cell.fg, Color::Rgb(..)) { cell.fg } else { INK };
            cell.set_fg(theme::lerp(base, SCROLL_DEPTH, k));
        }
    }
}

/// The gutter fills like a pipe: rows below the head are blank, the head is
/// bright, and rows above it get their plain `│` back.
pub(super) fn gutter_fill(ms: u32, color: Color) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::QuadOut), move |_, ctx, buf| {
        let a = ctx.area;
        if a.width <= GUTTER_X {
            return;
        }
        let reach = ctx.timer.alpha() * a.height as f32;
        for dy in 0..a.height {
            let d = reach - dy as f32;
            let cell = &mut buf[(a.x + GUTTER_X, a.y + dy)];
            if d < 0.0 {
                cell.set_symbol(" ");
            } else if d < 2.5 && ctx.timer.alpha() < 1.0 {
                cell.set_fg(theme::lerp(WHITE, color, d / 2.5)).set_symbol(if d < 1.0 { "╽" } else { "┃" });
            } else {
                cell.set_fg(theme::FAINT).set_symbol("│");
            }
        }
    })
}

/// A panel's border draws itself: two arms grow from the bottom middle and
/// meet at the top, each with a bright head cooling into `color`. Cells the
/// arms have not reached are blank, titles included.
pub(super) fn border_draw(ms: u32, color: Color) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::CubicOut), move |_, ctx, buf| {
        let a = ctx.area;
        let p = perimeter(a);
        let n = p.len() as f32;
        let reach = ctx.timer.alpha() * (n / 2.0 + 1.0);
        for (i, pos) in p.iter().enumerate() {
            let d = (i as f32).min(n - i as f32);
            let cell = &mut buf[*pos];
            if d > reach {
                cell.set_symbol(" ").set_bg(Color::Reset);
            } else if reach - d < 3.0 && ctx.timer.alpha() < 1.0 {
                cell.set_fg(theme::lerp(WHITE, color, (reach - d) / 3.0));
            }
        }
    })
}

/// `a`'s edge cells, clockwise from the bottom middle.
fn perimeter(a: Rect) -> Vec<(u16, u16)> {
    if a.width < 2 || a.height < 2 {
        return Vec::new();
    }
    let (l, r, top, bot) = (a.x, a.x + a.width - 1, a.y, a.y + a.height - 1);
    let mid = a.x + a.width / 2;
    let mut p: Vec<(u16, u16)> = (l..=mid).rev().map(|x| (x, bot)).collect();
    p.extend((top..bot).rev().map(|y| (l, y)));
    p.extend((l + 1..=r).map(|x| (x, top)));
    p.extend((top + 1..=bot).map(|y| (r, y)));
    p.extend((mid + 1..r).rev().map(|x| (x, bot)));
    p
}

/// A short horizontal shake that settles: each row in the rect shifts by a
/// decaying offset. For a failed tool's header.
pub(super) fn shake(ms: u32) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::Linear), |_, ctx, buf| {
        let a = ctx.area;
        let t = ctx.timer.alpha();
        let off = ((t * 28.0).sin() * 2.0 * (1.0 - t)).round() as i32;
        if off == 0 {
            return;
        }
        for y in a.y..a.y + a.height {
            let row: Vec<Cell> = (a.x..a.x + a.width).map(|x| buf[(x, y)].clone()).collect();
            for x in 0..a.width as i32 {
                let cell = &mut buf[(a.x + x as u16, y)];
                match usize::try_from(x - off).ok().and_then(|s| row.get(s)) {
                    Some(src) => *cell = src.clone(),
                    None => {
                        cell.set_symbol(" ");
                    }
                }
            }
        }
    })
}

/// An answered panel, gone from the layout, dissolving into what is now
/// drawn beneath it. `cells` is the panel as last drawn, row by row over the
/// rect this effect is given; `sel` is the chosen row range relative to the
/// panel's top edge. The chosen rows pop bright for a beat while the rest of
/// the panel washes to `tint` (green for allowed, amber for always/denied, red
/// for peer deny) and drops out cell by cell in a fixed scatter.
pub(super) fn ghost(cells: Vec<Cell>, sel: Option<Range<u16>>, tint: Color, ms: u32) -> Effect {
    let pop = if tint == theme::GREEN {
        FLASH_OK
    } else if tint == theme::RED {
        FLASH_ERR
    } else {
        FLASH_DENY
    };
    fx::effect_fn_buf(cells, (ms, Interpolation::QuadIn), move |cells, ctx, buf| {
        let a = ctx.area;
        let t = ctx.timer.alpha();
        for (i, c) in cells.iter().enumerate() {
            let (dx, dy) = ((i % a.width as usize) as u16, (i / a.width as usize) as u16);
            if dy >= a.height {
                continue;
            }
            let chosen = sel.as_ref().is_some_and(|r| r.contains(&dy)) && dx >= 1 && dx + 1 < a.width;
            let drop_t = if chosen { ((t - 0.22) / 0.78).max(0.0) } else { t };
            if scatter(dx, dy) < drop_t {
                continue;
            }
            let mut c = c.clone();
            if chosen && !c.symbol().trim().is_empty() {
                c.set_fg(theme::lerp(pop, tint, (t / 0.4).min(1.0)));
            } else if matches!(c.bg, Color::Rgb(..)) && c.bg != theme::SELECTED_BG {
                // Filled title badge: wash its background toward tint and keep its dark text legible.
                c.set_bg(theme::lerp(tint, c.bg, ((t - 0.15) / 0.5).max(0.0)));
            } else if let Color::Rgb(..) = c.fg {
                c.set_fg(theme::lerp(tint, c.fg, ((t - 0.15) / 0.5).max(0.0)));
            }
            if let Some(cell) = buf.cell_mut((a.x + dx, a.y + dy)) {
                *cell = c;
            }
        }
    })
}

/// A fixed pseudo-random threshold in [0, 1) per cell.
fn scatter(x: u16, y: u16) -> f32 {
    let (x, y) = (u32::from(x) + 1, u32::from(y) + 1);
    let h = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(2_246_822_519)).wrapping_mul(3_266_489_917);
    (h >> 8) as f32 / (1u32 << 24) as f32
}
