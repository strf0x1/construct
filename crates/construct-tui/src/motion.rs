//! Motion: short effects drawn over a finished frame with tachyonfx (ADR
//! 0064). A finished tool's output unfurls (the gutter fills top to bottom,
//! the text trickles in, the header flashes its outcome) and a comet rides
//! the rule above the input while a turn runs, colored by what it is doing.
//! Effects only recolor or blank cells already drawn, never set a
//! background (construct draws on the terminal's own), and never move
//! layout. `CONSTRUCT_MOTION=off` draws exactly what construct drew before.

use super::theme;
use super::turn::Phase;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::time::Instant;
use tachyonfx::fx;
use tachyonfx::pattern::SweepPattern;
use tachyonfx::{Duration, Effect, EffectRenderer, Interpolation};

const WHITE: Color = Color::Rgb(235, 238, 255);
const FLASH_OK: Color = Color::Rgb(230, 255, 210);
const FLASH_ERR: Color = Color::Rgb(255, 200, 210);
/// The `│` of a tool's output gutter (`    │ `), counted from the left edge.
const GUTTER_X: u16 = 4;

/// What an effect is drawn over. Blocks are named by index in `App::blocks`,
/// and their screen rows are looked up each frame, so an effect follows its
/// block as the transcript scrolls.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Head(usize),
    Body(usize),
}

pub(super) struct Motion {
    on: bool,
    fx: Vec<(Target, Effect)>,
    /// The comet and the color it was made for.
    comet: Option<(Color, Effect)>,
    /// One flash on the rule as the comet docks.
    dock: Option<Effect>,
    last: Instant,
    /// This frame's tick, taken once by `transcript` and reused by `rule`.
    frame: Duration,
}

/// Off: what `std::mem::take` leaves behind while a frame draws.
impl Default for Motion {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Motion {
    pub(super) fn new(on: bool) -> Self {
        Self { on, fx: Vec::new(), comet: None, dock: None, last: Instant::now(), frame: Duration::from_millis(0) }
    }

    /// On unless `CONSTRUCT_MOTION` is `off`, `0` or `false`.
    pub(super) fn from_env() -> Self {
        let off = std::env::var("CONSTRUCT_MOTION").is_ok_and(|v| matches!(v.trim(), "off" | "0" | "false"));
        Self::new(!off)
    }

    pub(super) fn on(&self) -> bool {
        self.on
    }

    /// Something is moving, so the loop keeps drawing.
    pub(super) fn running(&self) -> bool {
        !self.fx.is_empty() || self.comet.is_some() || self.dock.is_some()
    }

    /// Tool block `i` finished: unfurl its output and flash its outcome.
    pub(super) fn tool_done(&mut self, i: usize, ok: bool) {
        if !self.on {
            return;
        }
        self.wake();
        let flash = if ok { FLASH_OK } else { FLASH_ERR };
        self.fx.push((Target::Head(i), fx::fade_from_fg(flash, (700, Interpolation::QuadOut))));
        // coalesce runs its sweep backwards: down_to_up reveals the top row first.
        let trickle = fx::coalesce((700, Interpolation::QuadOut)).with_pattern(SweepPattern::down_to_up(3));
        self.fx.push((Target::Body(i), trickle));
        self.fx.push((Target::Body(i), gutter_fill(450, if ok { theme::ACCENT } else { theme::RED })));
    }

    /// The turn's phase this frame (`None` when idle): swap the comet to the
    /// phase's color, or dock it with one flash when the turn ends.
    pub(super) fn turn(&mut self, phase: Option<Option<&Phase>>) {
        if !self.on {
            return;
        }
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

    /// Draw this frame's effects over the transcript. `rows` holds each
    /// block's (first row, rows) in the transcript, `top` the first
    /// transcript row on screen, drawn at screen row `y0`, `h` rows tall.
    pub(super) fn transcript(&mut self, buf: &mut Buffer, rows: &[(usize, usize)], top: usize, y0: u16, h: u16) {
        let dt = self.tick();
        let area = buf.area;
        let place = |start: usize, len: usize| -> Option<Rect> {
            let (lo, hi) = (start.max(top), (start + len).min(top + h as usize));
            (hi > lo).then(|| Rect::new(area.x, y0 + (lo - top) as u16, area.width, (hi - lo) as u16))
        };
        for (target, e) in &mut self.fx {
            let rect = match *target {
                Target::Head(i) => rows.get(i).and_then(|r| place(r.0, 1)),
                Target::Body(i) => rows.get(i).and_then(|r| place(r.0 + 1, r.1.saturating_sub(1))),
            };
            if let Some(r) = rect.map(|r| r.intersection(area)).filter(|r| !r.is_empty()) {
                buf.render_effect(e, r, dt);
            } else {
                // off screen: let it run out rather than replay when scrolled back
                e.process(dt, &mut Buffer::empty(Rect::new(0, 0, 1, 1)), Rect::new(0, 0, 1, 1));
            }
        }
        self.fx.retain(|(_, e)| e.running());
    }

    /// Draw the comet, or the dock flash, over the rule.
    pub(super) fn rule(&mut self, buf: &mut Buffer, rect: Rect) {
        let rect = rect.intersection(buf.area);
        let dt = self.frame;
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

    /// Nothing ran since the last frame: start the clock now, so a new
    /// effect does not skip ahead by however long construct sat idle.
    fn wake(&mut self) {
        if !self.running() {
            self.last = Instant::now();
        }
    }

    /// Time since the last frame, capped so a stalled frame does not jump.
    fn tick(&mut self) -> Duration {
        let now = Instant::now();
        let ms = now.duration_since(self.last).as_millis().min(100) as u32;
        self.last = now;
        self.frame = Duration::from_millis(ms);
        self.frame
    }
}

/// The comet's color and lap time for a turn's phase; also the activity
/// line's highlight, so the two always agree.
pub(super) fn hue(phase: Option<&Phase>) -> (Color, u32) {
    match phase {
        Some(Phase::Routing) => (theme::CYAN, 1600),
        Some(Phase::Waiting) => (theme::ACCENT, 1900),
        Some(Phase::Thinking) => (theme::ACCENT2, 1900),
        Some(Phase::Writing) => (theme::ACCENT, 1600),
        Some(Phase::Tool(_)) => (theme::YELLOW, 1200),
        None => (theme::DIM, 1900),
    }
}

/// A bright head with a fading tail, lapping the rule left to right.
fn comet(color: Color, period_ms: u32) -> Effect {
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

/// The gutter fills like a pipe: rows below the head are blank, the head is
/// bright, and rows above it get their plain `│` back.
fn gutter_fill(ms: u32, color: Color) -> Effect {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(w: u16) -> Buffer {
        let mut b = Buffer::empty(Rect::new(0, 0, w, 1));
        for x in 0..w {
            b[(x, 0)].set_symbol("─").set_fg(theme::FAINT);
        }
        b
    }

    #[test]
    fn off_adds_nothing() {
        let mut m = Motion::new(false);
        m.tool_done(0, true);
        m.turn(Some(Some(&Phase::Thinking)));
        assert!(!m.running());
    }

    #[test]
    fn comet_follows_the_phase_and_docks() {
        let mut m = Motion::new(true);
        m.turn(Some(Some(&Phase::Thinking)));
        assert_eq!(m.comet.as_ref().map(|c| c.0), Some(theme::ACCENT2));
        m.turn(Some(Some(&Phase::Tool("bash".into()))));
        assert_eq!(m.comet.as_ref().map(|c| c.0), Some(theme::YELLOW));
        m.turn(None);
        assert!(m.comet.is_none() && m.dock.is_some());
    }

    #[test]
    fn comet_lights_the_rule() {
        let mut m = Motion::new(true);
        m.turn(Some(Some(&Phase::Writing)));
        let mut b = rule(80);
        m.frame = Duration::from_millis(800);
        let area = b.area;
        m.rule(&mut b, area);
        let head = (0..80).filter(|x| b[(*x, 0)].symbol() == "━").count();
        assert_eq!(head, 1, "one bright head");
        assert!((0..80).any(|x| b[(x, 0)].fg != theme::FAINT && b[(x, 0)].symbol() == "─"), "a tail");
    }

    #[test]
    fn unfurl_hides_rows_not_yet_reached_and_finishes() {
        let mut m = Motion::new(true);
        m.tool_done(0, true);
        let mut b = Buffer::empty(Rect::new(0, 0, 20, 4));
        let draw = |b: &mut Buffer| {
            for y in 1..4 {
                b.set_string(0, y, "    │ output line", ratatui::style::Style::new());
            }
        };
        draw(&mut b);
        m.last = Instant::now() - std::time::Duration::from_millis(20);
        m.transcript(&mut b, &[(0, 4)], 0, 0, 4);
        assert_eq!(b[(GUTTER_X, 3)].symbol(), " ", "the last row's gutter is not filled yet");
        for _ in 0..12 {
            draw(&mut b);
            m.last = Instant::now() - std::time::Duration::from_millis(100);
            m.transcript(&mut b, &[(0, 4)], 0, 0, 4);
        }
        assert!(!m.running(), "the unfurl runs out");
        assert_eq!(b[(GUTTER_X, 3)].symbol(), "│");
        assert_eq!(b[(6, 3)].symbol(), "o");
    }
}
