//! Drive a TUI in a real pseudo-terminal and observe its screen.
//!
//! Script lines (one per line, # comments):
//!   wait <text> [timeout_ms]   block until <text> is on screen (fails on timeout)
//!   gone <text> [timeout_ms]   block until <text> is no longer on screen
//!   type <text>                type literal text (\n is not special; use key enter)
//!   key <name>                 enter esc tab backspace up down left right pgup pgdn
//!                              ctrl-<c> alt-enter
//!   sleep <ms>
//!   snap [--style] [label]     print the screen; --style marks colored and
//!                              bold/dim/inverse runs as ⟦fg=#7aa2f7 bold⟧text⟦/⟧
//!   expect <text>              fail unless <text> is on screen now
//!   expect-style <text> <attrs>  fail unless every glyph of <text> has <attrs>:
//!                              fg=#rrggbb|0-255|default bg=… bold dim italic
//!                              underline inverse, ! to negate (`!bold`)
//!   image <path>               write the screen as SVG, or PNG if path ends .png
//!   resize <cols> <rows>
//!   settle <quiet_ms> [timeout_ms]  wait until the screen is quiet for quiet_ms;
//!                              records when it last changed ("settled")
//!
//! Timings (first output, first non-blank frame, each wait) are reported so
//! the same driver measures load latency in the bench.

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::style;

pub struct Driver {
    parser: vt100::Parser,
    rx: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pub start: Instant,
    pub first_output: Option<Duration>,
    pub first_frame: Option<Duration>,
    pub bytes: usize,
    /// When the screen last received output.
    pub last_output: Option<Duration>,
    carry: Vec<u8>,
    closed: bool,
}

impl Driver {
    pub fn spawn(cmd: &[String], env: &[(String, String)], cwd: Option<&str>, cols: u16, rows: u16) -> Result<Self> {
        let pty = NativePtySystem::default();
        let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut b = CommandBuilder::new(&cmd[0]);
        b.args(&cmd[1..]);
        b.env("TERM", "xterm-256color");
        b.env("COLORTERM", "truecolor");
        // An empty value unsets the variable, so callers can keep the parent's
        // settings (CONSTRUCT_MODEL, …) from leaking into the child.
        for (k, v) in env {
            if v.is_empty() {
                b.env_remove(k);
            } else {
                b.env(k, v);
            }
        }
        if let Some(d) = cwd {
            b.cwd(d);
        } else if let Ok(d) = std::env::current_dir() {
            b.cwd(d);
        }
        let start = Instant::now();
        let child = pair.slave.spawn_command(b).context("spawning under pty")?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (tx, rx) = mpsc::channel();
        let raw_log = std::env::var_os("DRIVE_RAW_LOG").and_then(|p| std::fs::File::create(p).ok());
        std::thread::spawn(move || {
            let mut raw_log = raw_log;
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Some(f) = raw_log.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                        }
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            parser: vt100::Parser::new(nz(rows), nz(cols), 0),
            rx,
            writer,
            master: pair.master,
            child,
            start,
            first_output: None,
            first_frame: None,
            bytes: 0,
            last_output: None,
            carry: Vec::new(),
            closed: false,
        })
    }

    /// Feed pending output into the screen. Returns false once the pty closed.
    /// Blocks up to `wait` for the first chunk, then drains whatever else is
    /// already queued without blocking.
    pub fn pump(&mut self, wait: Duration) -> bool {
        match self.rx.recv_timeout(wait) {
            Ok(b) => {
                self.ingest(&b);
                while let Ok(b) = self.rx.try_recv() {
                    self.ingest(&b);
                }
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout) => true,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.closed = true;
                false
            }
        }
    }

    fn ingest(&mut self, b: &[u8]) {
        if self.first_output.is_none() {
            self.first_output = Some(self.start.elapsed());
        }
        self.bytes += b.len();
        self.last_output = Some(self.start.elapsed());
        self.parser.process(b);
        self.answer_queries(b);
        if self.first_frame.is_none() && !self.text().trim().is_empty() {
            self.first_frame = Some(self.start.elapsed());
        }
    }

    /// Answer terminal queries the way a modern terminal (Ghostty-like)
    /// would, so programs that probe capabilities don't sit on timeouts that
    /// a real terminal would never trigger.
    fn answer_queries(&mut self, chunk: &[u8]) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(chunk);
        let mut replies: Vec<Vec<u8>> = Vec::new();
        let mut i = 0;
        let mut consumed = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                i += 1;
                continue;
            }
            let rest = &buf[i..];
            // CSI ... final byte, or OSC ... BEL/ST
            let (len, reply): (usize, Option<Vec<u8>>) = if rest.starts_with(b"\x1b[") {
                match rest[2..].iter().position(|b| (0x40..=0x7e).contains(b)) {
                    None => break, // incomplete: keep for the next chunk
                    Some(p) => {
                        let body = &rest[2..2 + p];
                        let fin = rest[2 + p];
                        let r = match (body, fin) {
                            (b"6", b'n') => {
                                let (r, c) = self.parser.screen().cursor_position();
                                Some(format!("\x1b[{};{}R", r + 1, c + 1).into_bytes())
                            }
                            (b"", b'c') | (b"0", b'c') => Some(b"\x1b[?62;22c".to_vec()),
                            (b">", b'c') | (b">0", b'c') => Some(b"\x1b[>1;10;0c".to_vec()),
                            (b">", b'q') | (b">0", b'q') => Some(b"\x1bP>|ghostty 1.2.0\x1b\\".to_vec()),
                            (b"?", b'u') => Some(b"\x1b[?0u".to_vec()),
                            (b"14", b't') => Some(b"\x1b[4;720;1200t".to_vec()),
                            (b"16", b't') => Some(b"\x1b[6;20;10t".to_vec()),
                            (b"18", b't') => {
                                let (rows, cols) = self.parser.screen().size();
                                Some(format!("\x1b[8;{rows};{cols}t").into_bytes())
                            }
                            (body, b'p') if body.starts_with(b"?") && body.ends_with(b"$") => {
                                let mode = String::from_utf8_lossy(&body[1..body.len() - 1]).to_string();
                                Some(format!("\x1b[?{mode};2$y").into_bytes())
                            }
                            _ => None,
                        };
                        (2 + p + 1, r)
                    }
                }
            } else if rest.starts_with(b"\x1b]") {
                let end = rest
                    .iter()
                    .position(|&b| b == 0x07)
                    .map(|p| (p, p + 1))
                    .or_else(|| rest.windows(2).position(|w| w == b"\x1b\\").map(|p| (p, p + 2)));
                match end {
                    None => break,
                    Some((body_end, total)) => {
                        let body = &rest[2..body_end];
                        let r = match body {
                            b"10;?" => Some(b"\x1b]10;rgb:c0c0/caca/f5f5\x1b\\".to_vec()),
                            b"11;?" => Some(b"\x1b]11;rgb:1a1a/1b1b/2626\x1b\\".to_vec()),
                            b"12;?" => Some(b"\x1b]12;rgb:c0c0/caca/f5f5\x1b\\".to_vec()),
                            b if b.starts_with(b"4;") && b.ends_with(b";?") => {
                                let idx = String::from_utf8_lossy(&b[2..b.len() - 2]).to_string();
                                Some(format!("\x1b]4;{idx};rgb:8080/8080/8080\x1b\\").into_bytes())
                            }
                            _ => None,
                        };
                        (total, r)
                    }
                }
            } else if rest.starts_with(b"\x1bP") {
                // DCS: XTGETTCAP (+q) — report the capability as unknown
                match rest.windows(2).position(|w| w == b"\x1b\\") {
                    None => break,
                    Some(p) => {
                        let body = &rest[2..p];
                        let r = body.starts_with(b"+q").then(|| b"\x1bP0+r\x1b\\".to_vec());
                        (p + 2, r)
                    }
                }
            } else if rest.len() < 2 {
                break;
            } else {
                (1, None)
            };
            if let Some(r) = reply {
                replies.push(r);
            }
            i += len;
            consumed = i;
        }
        // keep an unfinished escape sequence for the next chunk
        let tail_from = if i < buf.len() && buf[i] == 0x1b { i } else { buf.len() };
        let _ = consumed;
        self.carry = buf[tail_from..].to_vec();
        if self.carry.len() > 256 {
            self.carry.clear();
        }
        for r in replies {
            let _ = self.writer.write_all(&r);
        }
        let _ = self.writer.flush();
    }

    pub fn text(&self) -> String {
        self.parser.screen().contents()
    }

    pub fn wait_for(&mut self, needle: &str, timeout: Duration, present: bool) -> Result<Duration> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.text().contains(needle) == present {
                return Ok(self.start.elapsed());
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for {}{needle:?}\n--- screen ---\n{}",
                    if present { "" } else { "absence of " },
                    self.text()
                );
            }
            if !self.pump(Duration::from_millis(5)) && self.text().contains(needle) != present {
                bail!("program exited while waiting for {needle:?}\n--- screen ---\n{}", self.text());
            }
        }
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn key(&mut self, name: &str) -> Result<()> {
        let b: Vec<u8> = match name {
            "enter" => b"\r".to_vec(),
            "esc" => b"\x1b".to_vec(),
            "tab" => b"\t".to_vec(),
            "backspace" => b"\x7f".to_vec(),
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "right" => b"\x1b[C".to_vec(),
            "left" => b"\x1b[D".to_vec(),
            "pgup" => b"\x1b[5~".to_vec(),
            "pgdn" => b"\x1b[6~".to_vec(),
            "alt-enter" => b"\x1b\r".to_vec(),
            k if k.starts_with("ctrl-") && k.len() == 6 => {
                let c = k.as_bytes()[5].to_ascii_lowercase();
                vec![c - b'a' + 1]
            }
            other => bail!("unknown key {other}"),
        };
        self.send(&b)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        self.parser.screen_mut().set_size(nz(rows), nz(cols));
        Ok(())
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    pub fn exited(&mut self) -> bool {
        self.closed || matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].replace("\\\"", "\"")
    } else {
        s.to_string()
    }
}

/// Split `"text with spaces" 3000` into (text, Some(3000)).
fn text_and_timeout(rest: &str) -> (String, Option<u64>) {
    let rest = rest.trim();
    if rest.starts_with('"')
        && let Some(end) = rest[1..].find('"')
    {
        let text = rest[1..end + 1].to_string();
        let t = rest[end + 2..].trim().parse().ok();
        return (text, t);
    }
    match rest.rsplit_once(' ') {
        Some((a, b)) if b.parse::<u64>().is_ok() => (unquote(a), b.parse().ok()),
        _ => (unquote(rest), None),
    }
}

/// Split `"two words" fg=#fff bold` (or `word fg=#fff bold`) into the
/// quoted or first word and the rest.
fn text_and_rest(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if let Some(body) = rest.strip_prefix('"')
        && let Some(end) = body.find('"')
    {
        return (body[..end].to_string(), body[end + 1..].trim().to_string());
    }
    let (a, b) = rest.split_once(' ').unwrap_or((rest, ""));
    (a.to_string(), b.trim().to_string())
}

pub fn run_script(d: &mut Driver, script: &str, out: &mut dyn Write) -> Result<Value> {
    let mut waits = vec![];
    for (n, raw) in script.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let res: Result<()> = (|| {
            match cmd {
                "wait" | "gone" => {
                    let (text, t) = text_and_timeout(rest);
                    let at = d.wait_for(&text, Duration::from_millis(t.unwrap_or(10_000)), cmd == "wait")?;
                    waits.push(json!({ "line": n + 1, "text": text, "ms": at.as_secs_f64() * 1000.0, "kind": cmd }));
                }
                "type" => d.send(unquote(rest).as_bytes())?,
                "key" => {
                    d.key(rest.trim())?;
                    d.pump(Duration::from_millis(1));
                }
                "sleep" => {
                    let ms: u64 = rest.trim().parse().map_err(|_| anyhow!("sleep needs ms"))?;
                    let end = Instant::now() + Duration::from_millis(ms);
                    while Instant::now() < end {
                        d.pump(end.saturating_duration_since(Instant::now()).min(Duration::from_millis(20)));
                    }
                }
                "snap" => {
                    d.pump(Duration::from_millis(30));
                    let (styled, label) = match rest.trim().strip_prefix("--style") {
                        Some(l) => (true, l),
                        None => (false, rest),
                    };
                    let text = if styled { style::styled_text(d.parser.screen()) } else { d.text() };
                    writeln!(
                        out,
                        "--- snap {} @{:.0}ms ---",
                        unquote(label),
                        d.start.elapsed().as_secs_f64() * 1000.0
                    )?;
                    for l in text.lines() {
                        writeln!(out, "{}", l.trim_end())?;
                    }
                    writeln!(out, "---")?;
                }
                "expect-style" => {
                    d.pump(Duration::from_millis(30));
                    let (text, spec) = text_and_rest(rest);
                    style::check(d.parser.screen(), &text, &style::Want::parse(&spec)?)
                        .map_err(|e| anyhow!("{e}\n--- screen ---\n{}", style::styled_text(d.parser.screen())))?;
                }
                "image" => {
                    d.pump(Duration::from_millis(30));
                    let path = std::path::PathBuf::from(unquote(rest));
                    style::write_image(d.parser.screen(), &path)?;
                    writeln!(out, "--- image {} ---", path.display())?;
                }
                "expect" => {
                    d.pump(Duration::from_millis(30));
                    let t = unquote(rest);
                    if !d.text().contains(&t) {
                        bail!("expected {t:?} on screen\n--- screen ---\n{}", d.text());
                    }
                }
                "settle" => {
                    let mut it = rest.split_whitespace();
                    let quiet: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(500);
                    let timeout: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(30_000);
                    let deadline = Instant::now() + Duration::from_millis(timeout);
                    loop {
                        d.pump(Duration::from_millis(10));
                        let last = d.last_output.unwrap_or_default();
                        let now = d.start.elapsed();
                        if d.first_frame.is_some() && now.saturating_sub(last) >= Duration::from_millis(quiet) {
                            waits.push(json!({ "line": n + 1, "kind": "settle", "ms": last.as_secs_f64() * 1000.0 }));
                            break;
                        }
                        if Instant::now() >= deadline {
                            bail!("screen never settled\n--- screen ---\n{}", d.text());
                        }
                    }
                }
                "resize" => {
                    let mut it = rest.split_whitespace();
                    let c = it.next().and_then(|x| x.parse().ok()).ok_or(anyhow!("resize cols rows"))?;
                    let r = it.next().and_then(|x| x.parse().ok()).ok_or(anyhow!("resize cols rows"))?;
                    d.resize(c, r)?;
                }
                other => bail!("unknown command {other}"),
            }
            Ok(())
        })();
        res.with_context(|| format!("script line {}: {raw}", n + 1))?;
    }
    Ok(json!({
        "first_output_ms": d.first_output.map(|t| t.as_secs_f64() * 1000.0),
        "first_frame_ms": d.first_frame.map(|t| t.as_secs_f64() * 1000.0),
        "bytes": d.bytes,
        "waits": waits,
    }))
}

fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).unwrap()
}
