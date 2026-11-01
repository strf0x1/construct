//! How big one tool result may be: a share of the prompt budget, so a single
//! result can't fill a small window, with a fixed ceiling that big windows
//! reach, split between a round's parallel calls. The agent sets it before it
//! runs a round (`Ctx::size_results`).

use super::Ctx;
use std::sync::atomic::Ordering;

/// The most a tool result may hold, at any window (chars).
pub const OUTPUT_MAX: usize = 30_000;
/// One result's share of the budget. A quarter makes the cap `OUTPUT_MAX`
/// from about a 32k window up at four bytes a token (#95).
const RESULT_SHARE: f64 = 0.25;
/// A whole round's share: parallel calls split it, since the newest round is
/// never cut and ten reads at a single result's cap can fill a small window.
const ROUND_SHARE: f64 = 0.5;

impl Ctx {
    /// Size the results of a round of `calls` calls to a prompt budget of
    /// `budget_tokens`, at the bytes per token the provider's counts showed
    /// (dense data can be under two), never under a page a model can use.
    pub fn size_results(&self, budget_tokens: u64, calls: usize, bytes_per_token: f64) {
        let bytes = budget_tokens as f64 * bytes_per_token;
        let one = (bytes * RESULT_SHARE).min(bytes * ROUND_SHARE / calls.max(1) as f64) as usize;
        self.output_max.store(one.clamp(1024, OUTPUT_MAX), Ordering::Relaxed);
    }
    /// The cap a result is held to (`cap_output`, read's page size).
    pub fn output_max(&self) -> usize {
        self.output_max.load(Ordering::Relaxed)
    }
}

/// Keep tool output inside the context budget. Long output keeps its head and
/// tail; the whole thing is spilled to a file the model can read or grep.
pub fn cap_output(ctx: &Ctx, kind: &str, text: &str) -> String {
    let max = ctx.output_max();
    if text.len() <= max {
        return text.to_string();
    }
    let dir = ctx.paths.spill_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{kind}-{}-{}.txt", std::process::id(), crate::skills::now_nanos()));
    let saved = std::fs::write(&path, text).is_ok();
    let head = crate::llm::truncate(text, max / 2);
    let lines = text.lines().count();
    format!(
        "{head}\n\n[… {} bytes, {lines} lines total{} …]\n\n{}",
        text.len(),
        if saved { format!("; full output in {}", path.display()) } else { String::new() },
        crate::text::tail(text, max / 3)
    )
}
