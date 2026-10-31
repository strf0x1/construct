//! The status line's counters: `ctx ▰▰▱▱▱ 37% · billed ↑487k ↓6.2k · cache 20% · $0.123`.
//!
//! The design choice: the window and the bill are separate readings, so
//! they read apart. `ctx` (how full the window is now) comes first with a
//! meter that warms as it fills; `billed` is every token sent and received
//! over the session, which grows far past the window because each request
//! resends the history. Labels are faint, numbers a soft gray, and only a
//! filling window ever gets loud.

use super::app::App;
use super::theme;
use crate::journal::{Totals, human};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// Cells in the `ctx` meter.
const METER: u64 = 5;

/// Below this width the counters drop the meter and the `billed` label, so
/// a toast or the model name keeps some room.
pub(super) const COMPACT_BELOW: usize = 100;

/// The counters; `ctx` once a request has said how big the prompt is.
/// `router`: the model is a router, so the window is a guess (`~37%`).
/// `compact`: `ctx 37% · ↑487k ↓6.2k · …`, for a narrow terminal.
pub(super) fn counters(t: &Totals, context_pct: u64, router: bool, compact: bool) -> Vec<Span<'static>> {
    let mut out = vec![];
    let sep = || Span::styled(" · ", theme::faint());
    if t.last_prompt > 0 {
        let c = ctx_color(context_pct);
        let filled = context_pct.div_ceil(100 / METER).min(METER) as usize;
        out.push(Span::styled("ctx", theme::faint()));
        if !compact {
            out.push(Span::raw(" "));
            out.push(Span::styled("▰".repeat(filled), Style::new().fg(c)));
            let empty = theme::lerp(theme::FAINT, theme::DIM, 0.5);
            out.push(Span::styled("▱".repeat(METER as usize - filled), Style::new().fg(empty)));
        }
        out.push(Span::styled(
            format!(" {}", crate::journal::share(context_pct, t.last_prompt, router)),
            Style::new().fg(c),
        ));
        out.push(sep());
    }
    let u = &t.usage;
    if !compact {
        out.push(Span::styled("billed ", theme::faint()));
    }
    out.push(Span::styled("↑", theme::faint()));
    out.push(Span::styled(human(u.prompt_tokens), soft()));
    out.push(Span::styled(" ↓", theme::faint()));
    out.push(Span::styled(human(u.completion_tokens), soft()));
    out.push(sep());
    out.push(Span::styled("cache ", theme::faint()));
    let hit = u.hit_rate();
    out.push(Span::styled(
        format!("{:.0}%", hit * 100.0),
        Style::new().fg(theme::lerp(theme::SOFT, theme::CYAN, hit as f32)),
    ));
    if let Some(c) = u.cost {
        out.push(sep());
        let cost = if c == 0.0 { "$0".to_string() } else { format!("${c:.3}") };
        out.push(Span::styled(cost, Style::new().fg(theme::lerp(theme::SOFT, theme::GREEN, 0.55))));
    }
    out
}

fn soft() -> Style {
    Style::new().fg(theme::SOFT)
}

/// Calm blue while there is room, soft amber from half full, amber once
/// compaction is near (it starts at 70% by default), red past 90%.
fn ctx_color(pct: u64) -> Color {
    match pct {
        0..50 => theme::lerp(theme::DIM, theme::ACCENT, 0.7),
        50..70 => theme::lerp(theme::DIM, theme::YELLOW, 0.6),
        70..90 => theme::YELLOW,
        _ => theme::RED,
    }
}

impl App {
    /// The bottom line. The counters and the session id are never dropped:
    /// the toast and then the group labels give way first, then the model
    /// name is cut. The counters are `status::counters`.
    pub(super) fn status(&self, width: usize, phase: f32) -> Line<'static> {
        let router = self.cfg.on_router();
        let compact = width < super::status::COMPACT_BELOW;
        let mut must = super::status::counters(&self.totals, self.context_pct, router, compact);
        if self.scroll > 0 {
            must.push(Span::styled(format!(" · scrolled {}", self.scroll), Style::new().fg(theme::CYAN)));
        }
        if !self.session.is_empty() {
            must.push(Span::styled(format!(" · {}", short_session(&self.session)), theme::faint()));
        }
        must.push(Span::raw(" "));
        let w = |v: &[Span<'static>]| v.iter().map(|s| s.content.width()).sum::<usize>();
        let must_w = w(&must);
        // What is left of the line after a minimal model name (" ◆ " + 8).
        let mut spare = width.saturating_sub(must_w + 11);
        let mut extras: Vec<Span<'static>> = vec![];
        let toast = if let Some((s, _)) = &self.learning {
            Some(Span::styled(format!("◇ {s}  "), Style::new().fg(theme::pulse(phase, theme::FAINT, theme::ACCENT2))))
        } else if let Some((t, c, at)) = &self.toast
            && at.elapsed() < Duration::from_secs(6)
        {
            Some(Span::styled(format!("◇ {}  ", crate::llm::truncate(t, 60)), Style::new().fg(*c)))
        } else {
            self.jobs_span(phase)
        };
        let groups =
            (!self.groups.is_empty()).then(|| Span::styled(format!("{}  ", self.groups.join(" ")), theme::faint()));
        // The toast is cut to what is left (it is a hint); groups show whole or not at all.
        if let Some(sp) = toast {
            let sw = sp.content.width();
            if sw <= spare {
                spare -= sw;
                extras.push(sp);
            } else if spare >= 12 {
                let keep: String = sp.content.chars().take(spare - 3).collect();
                spare = 0;
                extras.push(Span::styled(format!("{keep}…  "), sp.style));
            }
        }
        if let Some(sp) = groups
            && sp.content.width() <= spare
        {
            extras.push(sp);
        }
        let right_w = w(&extras) + must_w;
        let model_room = width.saturating_sub(right_w + 3 + 1);
        let model = self.cfg.qualified_model();
        let model = if model.width() <= model_room {
            model
        } else {
            let keep = model_room.saturating_sub(1);
            let mut m: String = model.chars().take(keep).collect();
            m.push('…');
            m
        };
        let left = vec![Span::styled(" ◆ ", Style::new().fg(theme::ACCENT2)), Span::styled(model, theme::dim())];
        let lw = w(&left);
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(width.saturating_sub(lw + right_w))));
        spans.extend(extras);
        spans.extend(must);
        Line::from(spans)
    }
}

/// A session id (`20261023-034723-c3ca`) without its date: enough to tell
/// sessions apart on the status line. `/status` has the whole id.
fn short_session(id: &str) -> String {
    id.split_once('-').map_or(id, |(_, rest)| rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn reads_window_then_bill() {
        let mut t = Totals::default();
        t.add(&Event::Usage { prompt: 48_000, completion: 6_200, cached: 12_000, cache_write: 0, cost: Some(0.1234) });
        assert_eq!(text(&counters(&t, 37, false, false)), "ctx ▰▰▱▱▱ 37% · billed ↑48.0k ↓6.2k · cache 25% · $0.123");
        assert_eq!(text(&counters(&t, 37, true, false)), "ctx ▰▰▱▱▱ ~37% · billed ↑48.0k ↓6.2k · cache 25% · $0.123");
        assert_eq!(text(&counters(&t, 37, false, true)), "ctx 37% · ↑48.0k ↓6.2k · cache 25% · $0.123");
    }

    #[test]
    fn before_any_request_there_is_no_window_and_free_is_zero() {
        assert_eq!(text(&counters(&Totals::default(), 0, false, false)), "billed ↑0 ↓0 · cache 0%");
        let mut t = Totals::default();
        t.add(&Event::Usage { prompt: 10, completion: 1, cached: 0, cache_write: 0, cost: Some(0.0) });
        assert_eq!(text(&counters(&t, 0, false, false)), "ctx ▱▱▱▱▱ <1% · billed ↑10 ↓1 · cache 0% · $0");
    }

    #[test]
    fn the_meter_fills_and_warms() {
        let mut t = Totals::default();
        t.add(&Event::Usage { prompt: 1, completion: 0, cached: 0, cache_write: 0, cost: None });
        let meter = |p| text(&counters(&t, p, false, false)).split(' ').nth(1).unwrap().to_string();
        assert_eq!(meter(1), "▰▱▱▱▱");
        assert_eq!(meter(100), "▰▰▰▰▰");
        assert_eq!(ctx_color(90), theme::RED);
        assert_eq!(ctx_color(75), theme::YELLOW);
        assert_ne!(ctx_color(60), ctx_color(10));
    }
}
