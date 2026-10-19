//! Sessions are append-only JSONL files under sessions/. Chatstrata's
//! `construct` source reads them, which is how past sessions become searchable.

use crate::config::Paths;
use crate::llm::{Message, Usage};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;

pub struct Log {
    pub id: String,
    pub path: PathBuf,
    file: Option<std::fs::File>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn new_id() -> String {
    let ms = now_ms();
    let secs = (ms / 1000) as i64;
    // Civil date from unix seconds (UTC), Howard Hinnant's algorithm.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{:04x}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60,
        (crate::skills::now_nanos() as u64 ^ std::process::id() as u64) & 0xffff
    )
}

impl Log {
    pub fn create(paths: &Paths, cwd: &str, model: &str) -> Result<Self> {
        let dir = paths.sessions_dir();
        std::fs::create_dir_all(&dir)?;
        let id = new_id();
        let path = dir.join(format!("{id}.jsonl"));
        let mut log = Self { id: id.clone(), path, file: None };
        log.write(json!({
            "type": "session",
            "id": id,
            "harness": "construct",
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": cwd,
            "model": model,
            "at": now_ms(),
        }));
        Ok(log)
    }

    pub fn open(paths: &Paths, id: &str) -> Result<(Self, Vec<Message>)> {
        let path = paths.sessions_dir().join(format!("{id}.jsonl"));
        let text = std::fs::read_to_string(&path)?;
        let mut msgs = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["type"] == "message" {
                if let Ok(m) = serde_json::from_value::<Message>(v["message"].clone()) {
                    msgs.push(m);
                }
            }
        }
        Ok((Self { id: id.to_string(), path, file: None }, msgs))
    }

    /// The newest session started in `cwd`, if any.
    pub fn latest(paths: &Paths, cwd: &str) -> Option<String> {
        let mut ids: Vec<String> = std::fs::read_dir(paths.sessions_dir())
            .ok()?
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".jsonl")).map(String::from))
            .collect();
        ids.sort();
        ids.into_iter().rev().find(|id| {
            std::fs::read_to_string(paths.sessions_dir().join(format!("{id}.jsonl")))
                .ok()
                .and_then(|t| t.lines().next().and_then(|l| serde_json::from_str::<Value>(l).ok()))
                .is_some_and(|v| v["cwd"] == cwd)
        })
    }

    fn write(&mut self, v: Value) {
        if self.file.is_none() {
            self.file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{v}");
        }
    }

    pub fn message(&mut self, m: &Message) {
        self.write(json!({ "type": "message", "at": now_ms(), "message": m }));
    }

    pub fn usage(&mut self, u: &Usage, model: &str) {
        self.write(json!({ "type": "usage", "at": now_ms(), "model": model, "usage": u }));
    }

    pub fn event(&mut self, kind: &str, data: Value) {
        self.write(json!({ "type": kind, "at": now_ms(), "data": data }));
    }
}
