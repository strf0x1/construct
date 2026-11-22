//! Motion: short effects drawn over a finished frame with tachyonfx (ADR
//! 0065). A finished tool's output unfurls (the gutter fills top to bottom,
//! the text trickles in, the header flashes its outcome; a failure shakes) and
//! a comet rides the rule above the input while a turn runs, colored by what
//! it is doing. A permission panel draws its own border as it arrives and,
//! once answered, dissolves into the screen beneath it. A reply being
//! written dries from bright to ink behind a caret.
//! Effects recolor or blank cells already drawn and never set a background
//! (construct draws on the terminal's own); the few that move layout (an
//! output opening, the panel rising) are row counts in `grow.rs` (ADR 0066).
//! `CONSTRUCT_MOTION=off` draws exactly what construct drew before.

mod compact;
mod grow;
mod ink;
mod panel;
mod rule;
mod shaders;
pub(crate) mod splash;

use super::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::time::Instant;
use tachyonfx::fx;
use tachyonfx::pattern::SweepPattern;
use tachyonfx::{Duration, Effect, EffectRenderer, Interpolation};

pub(crate) use grow::rise;
use ink::Ink;
use panel::{Panel, Part};
pub(crate) use rule::hue;
use shaders::{gutter_fill, shake};

const WHITE: Color = Color::Rgb(235, 238, 255);
const FLASH_OK: Color = Color::Rgb(230, 255, 210);
const FLASH_ERR: Color = Color::Rgb(255, 200, 210);
/// The `│` of a tool's output gutter (`    │ `), counted from the left edge.
const GUTTER_X: u16 = 4;

/// Where a block landed in the last transcript drawn: its first row (after
/// the blank line before it), how many rows, and how wide its widest line
/// is, so an effect's sweep spans the text rather than the whole row.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct Place {
    pub(crate) start: usize,
    pub(crate) len: usize,
    pub(crate) width: u16,
}

/// What an effect is drawn over. Blocks are named by index in `App::blocks`,
/// and their screen rows are looked up each frame, so an effect follows its
/// block as the transcript scrolls.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Head(usize),
    Body(usize),
    /// The whole block.
    Block(usize),
    /// Every transcript row above the block.
    Above(usize),
    /// A part of the splash (`splash::TITLE` …), drawn only while it shows.
    Splash(usize),
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
    /// The permission panel as last drawn, and its effects while it is up.
    panel: Option<Panel>,
    panel_fx: Vec<(Part, Effect)>,
    /// An answered panel dissolving away: its rect and the effect.
    ghost: Option<(Rect, Effect)>,
    /// What the next ghost washes to: set by `answered`, dim when a question
    /// went stale unanswered.
    tint: Color,
    /// The next panel drawn is a new question (the last one was answered).
    rearm: bool,
    /// The ctx gauge easing down after a drop: from, to, since.
    gauge: Option<(u64, u64, Instant)>,
    /// Tool blocks whose output is opening, and since when.
    opening: Vec<(usize, Instant)>,
    /// The permission panel rising since, and an answered one's room
    /// closing: its height, since.
    rise: Option<Instant>,
    sink: Option<(u16, Instant)>,
    /// The reply being written, drying, and whether the model is writing.
    ink: Option<Ink>,
    writing: bool,
    started: Instant,
}

/// Off: what `std::mem::take` leaves behind while a frame draws.
impl Default for Motion {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Motion {
    pub(super) fn new(on: bool) -> Self {
        Self {
            on,
            fx: Vec::new(),
            comet: None,
            dock: None,
            last: Instant::now(),
            frame: Duration::from_millis(0),
            panel: None,
            panel_fx: Vec::new(),
            ghost: None,
            tint: theme::DIM,
            rearm: false,
            gauge: None,
            opening: Vec::new(),
            rise: None,
            sink: None,
            ink: None,
            writing: false,
            started: Instant::now(),
        }
    }

    /// On unless `CONSTRUCT_MOTION` is `off`, `0` or `false`.
    pub(super) fn from_env() -> Self {
        let off = std::env::var("CONSTRUCT_MOTION").is_ok_and(|v| matches!(v.trim(), "off" | "0" | "false"));
        let mut m = Self::new(!off);
        if m.on {
            m.splash_intro();
        }
        m
    }

    pub(super) fn on(&self) -> bool {
        self.on
    }

    /// Something is moving, so the loop keeps drawing.
    pub(super) fn running(&self) -> bool {
        !self.fx.is_empty()
            || self.comet.is_some()
            || self.dock.is_some()
            || !self.panel_fx.is_empty()
            || self.ghost.is_some()
            || self.gauge.is_some_and(|g| g.2.elapsed() < compact::DRAIN)
            || self.moving()
            || self.drying()
    }

    /// Tool block `i` finished: unfurl its output and flash its outcome.
    pub(super) fn tool_done(&mut self, i: usize, ok: bool, diff: bool) {
        if !self.on {
            return;
        }
        self.wake();
        self.open(i);
        let flash = if ok { FLASH_OK } else { FLASH_ERR };
        self.fx.push((Target::Head(i), fx::fade_from_fg(flash, (700, Interpolation::QuadOut))));
        // coalesce runs its sweep backwards: down_to_up reveals the top row first.
        let trickle = fx::coalesce((700, Interpolation::QuadOut)).with_pattern(SweepPattern::down_to_up(3));
        self.fx.push((Target::Body(i), trickle));
        self.fx.push((Target::Body(i), gutter_fill(450, if ok { theme::ACCENT } else { theme::RED })));
        if ok && diff {
            // a highlighter pass over the diff once it has trickled in
            let pass =
                fx::fade_from_fg(WHITE, (1100, Interpolation::SineOut)).with_pattern(SweepPattern::left_to_right(14));
            self.fx.push((Target::Body(i), fx::delay(250, pass)));
        }
        if !ok {
            self.fx.push((Target::Head(i), shake(380)));
            let glitch = fx::hsl_shift_fg([40.0, 0.0, 25.0], (500, Interpolation::QuadOut)).reversed();
            self.fx.push((Target::Body(i), glitch));
        }
    }

    /// Draw this frame's effects over the transcript. `rows` holds where
    /// each block landed in the transcript (`Place`), `top` the first
    /// transcript row on screen, drawn at screen row `y0`, `h` rows tall.
    /// `splash`: the splash is on screen and `rows` are its parts.
    pub(super) fn transcript(&mut self, buf: &mut Buffer, rows: &[Place], top: usize, y0: u16, h: u16, splash: bool) {
        let dt = self.tick();
        let area = buf.area;
        // two columns of slack: a shake's shift, the caret after the text
        let place = |start: usize, len: usize, width: u16| -> Option<Rect> {
            let (lo, hi) = (start.max(top), (start + len).min(top + h as usize));
            let w = width.saturating_add(2).min(area.width);
            (hi > lo).then(|| Rect::new(area.x, y0 + (lo - top) as u16, w, (hi - lo) as u16))
        };
        let full = area.width;
        for (target, e) in &mut self.fx {
            let rect = match *target {
                Target::Splash(k) => rows.get(k).and_then(|r| place(r.start, r.len, r.width)).filter(|_| splash),
                _ if splash => None,
                Target::Head(i) => rows.get(i).and_then(|r| place(r.start, 1, r.width)),
                Target::Body(i) => rows.get(i).and_then(|r| place(r.start + 1, r.len.saturating_sub(1), r.width)),
                Target::Block(i) => rows.get(i).and_then(|r| place(r.start, r.len, r.width)),
                Target::Above(i) => rows.get(i).and_then(|r| place(0, r.start, full)),
            };
            if let Some(r) = rect.map(|r| r.intersection(area)).filter(|r| !r.is_empty()) {
                buf.render_effect(e, r, dt);
            } else {
                // off screen: let it run out rather than replay when scrolled back
                e.process(dt, &mut Buffer::empty(Rect::new(0, 0, 1, 1)), Rect::new(0, 0, 1, 1));
            }
        }
        self.fx.retain(|(t, e)| e.running() && (splash || !matches!(t, Target::Splash(_))));
        if let Some(i) = self.ink.as_ref().map(|ink| ink.block).filter(|_| !splash) {
            let rect = rows.get(i).and_then(|r| place(r.start, r.len, r.width)).map(|r| r.intersection(area));
            self.dry(buf, i, rect);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::Phase;

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
        m.tool_done(0, true, false);
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

    /// A drawn panel: rounded border, a selected row, text.
    fn panel_frame(b: &mut Buffer, area: Rect, sel: u16) {
        use ratatui::widgets::{Block, BorderType, Widget};
        Block::bordered().border_type(BorderType::Rounded).render(area, b);
        b.set_string(area.x + 2, area.y + 1, "bash wants to read", ratatui::style::Style::new());
        for x in area.x + 2..area.x + area.width - 2 {
            b[(x, area.y + 2 + sel)].set_bg(theme::SELECTED_BG);
        }
    }

    fn step(m: &mut Motion, ms: u64) {
        m.last = Instant::now() - std::time::Duration::from_millis(ms);
        let mut scratch = Buffer::empty(Rect::new(0, 0, 1, 1));
        m.transcript(&mut scratch, &[], 0, 0, 0, false);
    }

    #[test]
    fn a_new_panel_draws_its_border_from_the_bottom() {
        let mut m = Motion::new(true);
        let area = Rect::new(0, 0, 30, 6);
        let mut b = Buffer::empty(area);
        panel_frame(&mut b, area, 0);
        step(&mut m, 30);
        m.panel(&mut b, area);
        assert_eq!(b[(15, 0)].symbol(), " ", "the top middle is reached last");
        assert_eq!(b[(15, 5)].symbol(), "─", "the bottom middle is where it starts");
        for _ in 0..8 {
            panel_frame(&mut b, area, 0);
            step(&mut m, 100);
            m.panel(&mut b, area);
        }
        assert_eq!(b[(15, 0)].symbol(), "─");
        assert!(m.panel_fx.is_empty(), "the arrival runs out");
    }

    #[test]
    fn a_moved_selection_sweeps_once() {
        let mut m = Motion::new(true);
        let area = Rect::new(0, 0, 30, 6);
        let mut b = Buffer::empty(area);
        for _ in 0..8 {
            panel_frame(&mut b, area, 0);
            step(&mut m, 100);
            m.panel(&mut b, area);
        }
        let mut b = Buffer::empty(area);
        panel_frame(&mut b, area, 1);
        step(&mut m, 16);
        m.panel(&mut b, area);
        assert_eq!(m.panel_fx.iter().map(|(p, _)| *p).collect::<Vec<_>>(), [Part::Selected]);
    }

    #[test]
    fn an_answered_panel_dissolves_over_what_replaced_it() {
        let mut m = Motion::new(true);
        let area = Rect::new(0, 0, 30, 6);
        let mut b = Buffer::empty(area);
        for _ in 0..8 {
            panel_frame(&mut b, area, 0);
            step(&mut m, 100);
            m.panel(&mut b, area);
        }
        m.answered(true);
        let under = |b: &mut Buffer| b.set_string(0, 3, "› ask construct anything", ratatui::style::Style::new());
        let mut b = Buffer::empty(area);
        under(&mut b);
        step(&mut m, 16);
        m.rule(&mut b, Rect::new(0, 2, 30, 1));
        assert!(m.ghost.is_some());
        let edge = area.positions().filter(|p| ["╭", "╮", "╰", "╯", "─", "│"].contains(&b[*p].symbol())).count();
        assert!(edge > 50, "most of the panel is still there at first ({edge} edge cells)");
        assert_eq!(m.tint, theme::DIM, "the tint was used up");
        for _ in 0..6 {
            let mut fresh = Buffer::empty(area);
            under(&mut fresh);
            b = fresh;
            step(&mut m, 100);
            m.rule(&mut b, Rect::new(0, 2, 30, 1));
        }
        assert!(m.ghost.is_none(), "the ghost runs out");
        assert_eq!(b[(0, 0)].symbol(), " ");
        assert_eq!(b[(2, 3)].symbol(), "a", "what replaced it shows");
    }

    #[test]
    fn a_failure_shakes_its_header() {
        let mut m = Motion::new(true);
        m.tool_done(0, false, false);
        let mut moved = false;
        for _ in 0..10 {
            let mut b = Buffer::empty(Rect::new(0, 0, 20, 2));
            b.set_string(2, 0, "✗ bash", ratatui::style::Style::new());
            m.last = Instant::now() - std::time::Duration::from_millis(30);
            m.transcript(&mut b, &[Place { start: 0, len: 2, width: 20 }], 0, 0, 2, false);
            moved |= b[(2, 0)].symbol() != "✗";
        }
        assert!(moved, "the header shifted at least once");
    }

    #[test]
    fn unfurl_hides_rows_not_yet_reached_and_finishes() {
        let mut m = Motion::new(true);
        m.tool_done(0, true, false);
        m.opening.clear(); // the rows opening is grow.rs's to test, on the wall clock
        let mut b = Buffer::empty(Rect::new(0, 0, 20, 4));
        let draw = |b: &mut Buffer| {
            for y in 1..4 {
                b.set_string(0, y, "    │ output line", ratatui::style::Style::new());
            }
        };
        draw(&mut b);
        m.last = Instant::now() - std::time::Duration::from_millis(20);
        m.transcript(&mut b, &[Place { start: 0, len: 4, width: 20 }], 0, 0, 4, false);
        assert_eq!(b[(GUTTER_X, 3)].symbol(), " ", "the last row's gutter is not filled yet");
        for _ in 0..12 {
            draw(&mut b);
            m.last = Instant::now() - std::time::Duration::from_millis(100);
            m.transcript(&mut b, &[Place { start: 0, len: 4, width: 20 }], 0, 0, 4, false);
        }
        assert!(!m.running(), "the unfurl runs out");
        assert_eq!(b[(GUTTER_X, 3)].symbol(), "│");
        assert_eq!(b[(6, 3)].symbol(), "o");
    }
}
