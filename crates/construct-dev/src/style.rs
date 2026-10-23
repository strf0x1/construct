//! What a screen looks like, not just what it says: per-cell color and
//! attributes from the vt100 grid, as marked-up text (`snap --style`), as a
//! check (`expect-style`), and as an SVG or PNG image (`image`).
//!
//! The design choice: one markup an agent can read and grep, `⟦fg=#7aa2f7
//! bold⟧text⟦/⟧`, with plain text left bare, so a styled snapshot reads like
//! the plain one and a diff shows only what changed. Colors are spelled the
//! way the TUI declares them (`#rrggbb`, or the 0–255 index), so a check can
//! be written straight from `tui/theme.rs`.

use anyhow::{Result, bail};
use std::fmt::Write as _;

/// The colors the driver reports for the terminal's own foreground and
/// background (see `Driver::answer_queries`); the image uses them for
/// "default".
const DEFAULT_FG: (u8, u8, u8) = (0xc0, 0xca, 0xf5);
const DEFAULT_BG: (u8, u8, u8) = (0x1a, 0x1b, 0x26);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: vt100::Color,
    pub bg: vt100::Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Style {
    fn of(c: &vt100::Cell) -> Self {
        Self {
            fg: c.fgcolor(),
            bg: c.bgcolor(),
            bold: c.bold(),
            dim: c.dim(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
        }
    }

    fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    /// `fg=#7aa2f7 bg=4 bold` — only what differs from the default.
    pub fn attrs(&self) -> String {
        let mut v = vec![];
        if self.fg != vt100::Color::Default {
            v.push(format!("fg={}", color_name(self.fg)));
        }
        if self.bg != vt100::Color::Default {
            v.push(format!("bg={}", color_name(self.bg)));
        }
        for (on, name) in [
            (self.bold, "bold"),
            (self.dim, "dim"),
            (self.italic, "italic"),
            (self.underline, "underline"),
            (self.inverse, "inverse"),
        ] {
            if on {
                v.push(name.to_string());
            }
        }
        v.join(" ")
    }
}

fn color_name(c: vt100::Color) -> String {
    match c {
        vt100::Color::Default => "default".into(),
        vt100::Color::Idx(i) => i.to_string(),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn parse_color(s: &str) -> Result<vt100::Color> {
    if s == "default" {
        return Ok(vt100::Color::Default);
    }
    if let Some(hex) = s.strip_prefix('#')
        && hex.len() == 6
        && let Ok(n) = u32::from_str_radix(hex, 16)
    {
        return Ok(vt100::Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8));
    }
    match s.parse::<u8>() {
        Ok(i) => Ok(vt100::Color::Idx(i)),
        Err(_) => bail!("color {s:?}: use #rrggbb, 0-255 or default"),
    }
}

/// One screen row as (text, style, column) per glyph; wide glyphs take two
/// columns and appear once.
fn row(screen: &vt100::Screen, r: u16) -> Vec<(String, Style, u16)> {
    let (_, cols) = screen.size();
    (0..cols.get())
        .filter_map(|c| {
            let cell = screen.cell(r, c)?;
            if cell.is_wide_continuation() {
                return None;
            }
            let text = if cell.has_contents() { cell.contents().to_string() } else { " ".into() };
            Some((text, Style::of(cell), c))
        })
        .collect()
}

/// The screen with styled runs marked up and trailing plain blanks trimmed.
pub fn styled_text(screen: &vt100::Screen) -> String {
    let (rows, _) = screen.size();
    let mut out = String::new();
    for r in 0..rows.get() {
        let mut cells = row(screen, r);
        while cells.last().is_some_and(|(t, s, _)| t == " " && s.is_plain()) {
            cells.pop();
        }
        let mut line = String::new();
        let mut i = 0;
        while i < cells.len() {
            let style = cells[i].1;
            let mut text = String::new();
            while i < cells.len() && cells[i].1 == style {
                text.push_str(&cells[i].0);
                i += 1;
            }
            if style.is_plain() {
                line.push_str(&text);
            } else {
                let _ = write!(line, "⟦{}⟧{text}⟦/⟧", style.attrs());
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// What `expect-style` asks for: attributes that must hold (`bold`), must
/// not (`!bold`), and colors that must match (`fg=#7aa2f7`).
#[derive(Debug, Default)]
pub struct Want {
    fg: Option<vt100::Color>,
    bg: Option<vt100::Color>,
    flags: Vec<(&'static str, bool)>,
}

impl Want {
    pub fn parse(spec: &str) -> Result<Self> {
        let mut w = Self::default();
        for tok in spec.split_whitespace() {
            if let Some(c) = tok.strip_prefix("fg=") {
                w.fg = Some(parse_color(c)?);
                continue;
            }
            if let Some(c) = tok.strip_prefix("bg=") {
                w.bg = Some(parse_color(c)?);
                continue;
            }
            let (on, name) = match tok.strip_prefix('!') {
                Some(n) => (false, n),
                None => (true, tok),
            };
            let flag = match name {
                "bold" => "bold",
                "dim" => "dim",
                "italic" => "italic",
                "underline" => "underline",
                "inverse" => "inverse",
                other => bail!("unknown attribute {other:?}: fg=… bg=… bold dim italic underline inverse, ! to negate"),
            };
            w.flags.push((flag, on));
        }
        if w.fg.is_none() && w.bg.is_none() && w.flags.is_empty() {
            bail!("expect-style needs at least one attribute");
        }
        Ok(w)
    }

    fn mismatch(&self, s: &Style) -> Option<String> {
        let mut bad = vec![];
        if let Some(fg) = self.fg
            && fg != s.fg
        {
            bad.push(format!("fg is {}", color_name(s.fg)));
        }
        if let Some(bg) = self.bg
            && bg != s.bg
        {
            bad.push(format!("bg is {}", color_name(s.bg)));
        }
        for &(flag, on) in &self.flags {
            let has = match flag {
                "bold" => s.bold,
                "dim" => s.dim,
                "italic" => s.italic,
                "underline" => s.underline,
                _ => s.inverse,
            };
            if has != on {
                bad.push(format!("{}{flag}", if has { "" } else { "not " }));
            }
        }
        (!bad.is_empty()).then(|| bad.join(", "))
    }
}

/// Check every non-blank glyph of the first on-screen occurrence of
/// `needle` (within one row) against `want`.
pub fn check(screen: &vt100::Screen, needle: &str, want: &Want) -> Result<()> {
    let (rows, _) = screen.size();
    for r in 0..rows.get() {
        let cells = row(screen, r);
        let mut text = String::new();
        let mut starts = vec![];
        for (t, _, _) in &cells {
            starts.push(text.len());
            text.push_str(t);
        }
        let Some(at) = text.find(needle) else { continue };
        let first = starts.partition_point(|&s| s < at);
        let last = starts.partition_point(|&s| s < at + needle.len());
        for (t, s, _) in &cells[first..last] {
            if t.trim().is_empty() {
                continue;
            }
            if let Some(why) = want.mismatch(s) {
                bail!("{needle:?} on row {}: {why} (glyph {t:?} is ⟦{}⟧)", r + 1, s.attrs());
            }
        }
        return Ok(());
    }
    bail!("{needle:?} is not on screen")
}

/// The xterm palette for indexed colors.
fn idx_rgb(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match i {
        0..16 => BASE[i as usize],
        16..232 => {
            let n = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (step(n / 36), step((n / 6) % 6), step(n % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            (g, g, g)
        }
    }
}

fn rgb(c: vt100::Color, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => idx_rgb(i),
        vt100::Color::Rgb(r, g, b) => (r, g, b),
    }
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The screen as an SVG on a fixed cell grid, cursor included.
pub fn svg(screen: &vt100::Screen) -> String {
    const CW: u32 = 9;
    const CH: u32 = 18;
    let (rows, cols) = screen.size();
    let (w, h) = (cols.get() as u32 * CW, rows.get() as u32 * CH);
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" \
         font-family=\"DejaVu Sans Mono, Menlo, Consolas, monospace\" font-size=\"15\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"{}\"/>\n",
        hex(DEFAULT_BG)
    );
    for r in 0..rows.get() {
        let y = r as u32 * CH;
        for (t, s, c) in row(screen, r) {
            let (mut fg, mut bg) = (rgb(s.fg, DEFAULT_FG), rgb(s.bg, DEFAULT_BG));
            if s.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let x = c as u32 * CW;
            let span = if screen.cell(r, c).is_some_and(|cell| cell.is_wide()) { 2 } else { 1 };
            if bg != DEFAULT_BG {
                let _ = writeln!(
                    out,
                    "<rect x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{CH}\" fill=\"{}\"/>",
                    CW * span,
                    hex(bg)
                );
            }
            if t.trim().is_empty() {
                continue;
            }
            let mut attrs = format!("fill=\"{}\"", hex(fg));
            if s.bold {
                attrs.push_str(" font-weight=\"bold\"");
            }
            if s.italic {
                attrs.push_str(" font-style=\"italic\"");
            }
            if s.underline {
                attrs.push_str(" text-decoration=\"underline\"");
            }
            if s.dim {
                attrs.push_str(" opacity=\"0.6\"");
            }
            let _ = writeln!(out, "<text x=\"{x}\" y=\"{}\" {attrs}>{}</text>", y + CH - 4, escape(&t));
        }
    }
    if !screen.hide_cursor() {
        let (r, c) = screen.cursor_position();
        let _ = writeln!(
            out,
            "<rect x=\"{}\" y=\"{}\" width=\"{CW}\" height=\"{CH}\" fill=\"{}\" opacity=\"0.5\"/>",
            c as u32 * CW,
            r as u32 * CH,
            hex(DEFAULT_FG)
        );
    }
    out.push_str("</svg>\n");
    out
}

/// Write the screen to `path`: SVG, or PNG through `rsvg-convert` (or
/// ImageMagick) when the path ends in `.png`.
pub fn write_image(screen: &vt100::Screen, path: &std::path::Path) -> Result<()> {
    let svg = svg(screen);
    if path.extension().is_none_or(|e| e != "png") {
        std::fs::write(path, svg)?;
        return Ok(());
    }
    let tmp = path.with_extension("svg");
    std::fs::write(&tmp, &svg)?;
    let tries: [(&str, Vec<&std::ffi::OsStr>); 2] = [
        ("rsvg-convert", vec!["-o".as_ref(), path.as_os_str(), tmp.as_os_str()]),
        ("magick", vec![tmp.as_os_str(), path.as_os_str()]),
    ];
    for (prog, args) in tries {
        if let Ok(s) = std::process::Command::new(prog).args(args).status()
            && s.success()
        {
            let _ = std::fs::remove_file(&tmp);
            return Ok(());
        }
    }
    bail!("no SVG rasterizer: install rsvg-convert (librsvg) or ImageMagick; the SVG is at {}", tmp.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: u16) -> std::num::NonZeroU16 {
        std::num::NonZeroU16::new(n).unwrap()
    }

    fn screen(bytes: &[u8]) -> vt100::Parser {
        let mut p = vt100::Parser::new(nz(3), nz(30), 0);
        p.process(bytes);
        p
    }

    #[test]
    fn marks_up_styled_runs_and_leaves_plain_text_bare() {
        let p = screen(b"\x1b[1;38;2;122;162;247mAllow\x1b[0m once \x1b[7mDeny\x1b[0m");
        let first = styled_text(p.screen()).lines().next().unwrap().to_string();
        assert_eq!(first, "⟦fg=#7aa2f7 bold⟧Allow⟦/⟧ once ⟦inverse⟧Deny⟦/⟧");
    }

    #[test]
    fn checks_a_needle_against_attributes() {
        let p = screen(b"\x1b[33mwarn\x1b[0m ok");
        check(p.screen(), "warn", &Want::parse("fg=3 !bold").unwrap()).unwrap();
        let e = check(p.screen(), "ok", &Want::parse("fg=3").unwrap()).unwrap_err().to_string();
        assert!(e.contains("fg is default"), "{e}");
        assert!(check(p.screen(), "absent", &Want::parse("bold").unwrap()).is_err());
        assert!(Want::parse("sparkly").is_err());
    }

    #[test]
    fn svg_draws_backgrounds_and_escapes_text() {
        let p = screen(b"\x1b[41m<&>\x1b[0m");
        let s = svg(p.screen());
        assert!(s.contains("fill=\"#cd0000\""), "{s}");
        assert!(s.contains("&lt;") && s.contains("&amp;"), "{s}");
    }
}
