//! When a driven program's output arrived, measured from its spawn: what
//! `--metrics` reports and what `settle` waits on.
//!
//! The design choice: the first frame is the first output after which the
//! screen shows any text, not the first byte, since a program can write
//! escape sequences long before it draws anything.

use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(super) struct Timings {
    start: Instant,
    first_output: Option<Duration>,
    first_frame: Option<Duration>,
    last_output: Option<Duration>,
}

impl Timings {
    pub(super) fn new(start: Instant) -> Self {
        Self { start, first_output: None, first_frame: None, last_output: None }
    }

    /// Time since the program was spawned.
    pub(super) fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Output just arrived; `drawn` says whether the screen now shows text.
    pub(super) fn output(&mut self, drawn: impl FnOnce() -> bool) {
        let now = self.elapsed();
        self.first_output.get_or_insert(now);
        self.last_output = Some(now);
        if self.first_frame.is_none() && drawn() {
            self.first_frame = Some(now);
        }
    }

    /// When the screen last changed, once it has drawn and stayed quiet `quiet`.
    pub(super) fn settled(&self, quiet: Duration) -> Option<Duration> {
        let last = self.last_output.unwrap_or_default();
        (self.first_frame.is_some() && self.elapsed().saturating_sub(last) >= quiet).then_some(last)
    }

    /// What `--metrics` reports: startup timings, output volume and `waits`.
    pub(super) fn metrics(&self, bytes: usize, waits: Vec<Value>) -> Value {
        let ms = |t: Option<Duration>| t.map(|t| t.as_secs_f64() * 1000.0);
        json!({ "first_output_ms": ms(self.first_output), "first_frame_ms": ms(self.first_frame), "bytes": bytes, "waits": waits })
    }
}
