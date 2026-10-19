//! Colors. Foreground-only on the terminal's own background, so construct
//! sits inside whatever theme the user already runs.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(122, 162, 247); // soft blue
pub const ACCENT2: Color = Color::Rgb(187, 154, 247); // lavender
pub const GREEN: Color = Color::Rgb(158, 206, 106);
pub const RED: Color = Color::Rgb(247, 118, 142);
pub const YELLOW: Color = Color::Rgb(224, 175, 104);
pub const CYAN: Color = Color::Rgb(125, 207, 255);
pub const ORANGE: Color = Color::Rgb(255, 158, 100);
pub const DIM: Color = Color::Rgb(110, 118, 150);
pub const FAINT: Color = Color::Rgb(70, 76, 102);

pub fn dim() -> Style {
    Style::new().fg(DIM)
}
pub fn faint() -> Style {
    Style::new().fg(FAINT)
}
pub fn accent() -> Style {
    Style::new().fg(ACCENT)
}
pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn rgb(c: Color) -> (f32, f32, f32) {
    match c {
        Color::Rgb(r, g, b) => (r as f32, g as f32, b as f32),
        _ => (200.0, 200.0, 200.0),
    }
}

pub fn lerp(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    Color::Rgb((ar + (br - ar) * t) as u8, (ag + (bg - ag) * t) as u8, (ab + (bb - ab) * t) as u8)
}

/// A highlight that sweeps across `text`: each char brightens as the
/// highlight passes. `phase` is seconds since the animation started.
pub fn shimmer(text: &str, phase: f32, base: Color, hi: Color) -> Vec<ratatui::text::Span<'static>> {
    let n = text.chars().count().max(1) as f32;
    let period = 1.6f32;
    let pos = ((phase % period) / period) * (n + 8.0) - 4.0;
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let d = (i as f32 - pos).abs();
            let t = (1.0 - d / 4.0).max(0.0);
            ratatui::text::Span::styled(c.to_string(), Style::new().fg(lerp(base, hi, t * t)))
        })
        .collect()
}

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner(phase: f32) -> &'static str {
    SPINNER[((phase * 12.0) as usize) % SPINNER.len()]
}

/// A slow breathing pulse between two colors, for idle-but-alive states.
pub fn pulse(phase: f32, a: Color, b: Color) -> Color {
    let t = (phase * std::f32::consts::PI / 1.2).sin() * 0.5 + 0.5;
    lerp(a, b, t)
}
