//! Drive a TUI in a real pseudo-terminal and observe its screen.
//!
//! Script lines (one per line, # comments):
//!   wait <text> [timeout_ms]   block until <text> is on screen (fails on timeout)
//!   gone <text> [timeout_ms]   block until <text> is no longer on screen
//!   type <text>                type literal text (\n is not special; use key enter)
//!   key <name>                 enter esc tab backspace up down left right pgup pgdn
//!                              ctrl-<c> alt-enter
//!   sleep <ms>
//!   snap [label]               print the screen
//!   expect <text>              fail unless <text> is on screen now
//!   resize <cols> <rows>
//!
//! Timings (first output, first non-blank frame, each wait) are reported so
//! the same driver measures load latency in the bench.

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
        for (k, v) in env {
            b.env(k, v);
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
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
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
            closed: false,
        })
    }

    /// Feed pending output into the screen. Returns false once the pty closed.
    pub fn pump(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(b) => {
                    if self.first_output.is_none() {
                        self.first_output = Some(self.start.elapsed());
                    }
                    self.bytes += b.len();
                    self.parser.process(&b);
                    // Answer cursor-position queries so TUIs that ask don't hang.
                    if b.windows(4).any(|w| w == b"\x1b[6n") {
                        let (r, c) = self.parser.screen().cursor_position();
                        let _ = self.writer.write_all(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
                    }
                    if self.first_frame.is_none() && !self.text().trim().is_empty() {
                        self.first_frame = Some(self.start.elapsed());
                    }
                    // keep draining without blocking
                    while let Ok(b) = self.rx.try_recv() {
                        self.bytes += b.len();
                        self.parser.process(&b);
                        if self.first_frame.is_none() && !self.text().trim().is_empty() {
                            self.first_frame = Some(self.start.elapsed());
                        }
                    }
                    return true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return true,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.closed = true;
                    return false;
                }
            }
        }
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
                bail!("timed out waiting for {}{needle:?}\n--- screen ---\n{}", if present { "" } else { "absence of " }, self.text());
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
    if rest.starts_with('"') {
        if let Some(end) = rest[1..].find('"') {
            let text = rest[1..end + 1].to_string();
            let t = rest[end + 2..].trim().parse().ok();
            return (text, t);
        }
    }
    match rest.rsplit_once(' ') {
        Some((a, b)) if b.parse::<u64>().is_ok() => (unquote(a), b.parse().ok()),
        _ => (unquote(rest), None),
    }
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
                    writeln!(out, "--- snap {} @{:.0}ms ---", unquote(rest), d.start.elapsed().as_secs_f64() * 1000.0)?;
                    for l in d.text().lines() {
                        writeln!(out, "{}", l.trim_end())?;
                    }
                    writeln!(out, "---")?;
                }
                "expect" => {
                    d.pump(Duration::from_millis(30));
                    let t = unquote(rest);
                    if !d.text().contains(&t) {
                        bail!("expected {t:?} on screen\n--- screen ---\n{}", d.text());
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
