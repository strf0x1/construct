//! The one door to Jev (TypeSafe's System One model). Routing and the learning
//! gate both go through `ask`, which:
//! - keeps `state` under a byte budget, shortening the longest strings
//!   (head and tail kept) and saying so, so Jev treats missing text as unknown;
//! - stops calling Jev for a minute after three failures in a row, so one bad
//!   patch of network doesn't add a timeout to every turn;
//! - logs every call to `$CONSTRUCT_HOME/jev.log` (purpose, latency, bytes),
//!   which is how we check that Jev saves more than it costs.

use crate::config::{Paths, RouterConfig};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Bytes of JSON state we send before shortening. Jev weighs everything in
/// `state`; past a few KB it is mostly noise for the questions we ask.
const STATE_BUDGET: usize = 12_000;
const TRIP_AFTER: u32 = 3;
const OPEN_FOR_MS: u64 = 60_000;

static FAILURES: AtomicU32 = AtomicU32::new(0);
static OPEN_UNTIL: AtomicU64 = AtomicU64::new(0);

pub async fn ask(
    cfg: &RouterConfig,
    http: &reqwest::Client,
    key: &str,
    purpose: &str,
    mut state: Value,
    questions: Map<String, Value>,
    timeout: Duration,
) -> Result<Value, String> {
    let now = crate::journal::now_ms();
    if now < OPEN_UNTIL.load(Ordering::Relaxed) {
        return Err("skipped: jev failed 3 times in a row".into());
    }
    let shortened = fit(&mut state, STATE_BUDGET);
    let body = json!({ "model": cfg.jev_model, "state": state, "questions": questions });
    let bytes = body.to_string().len();
    let started = Instant::now();
    let result = send(http, &cfg.jev_url, key, &body, timeout).await;
    let ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => FAILURES.store(0, Ordering::Relaxed),
        Err(_) => {
            if FAILURES.fetch_add(1, Ordering::Relaxed) + 1 >= TRIP_AFTER {
                OPEN_UNTIL.store(now + OPEN_FOR_MS, Ordering::Relaxed);
                FAILURES.store(0, Ordering::Relaxed);
            }
        }
    }
    log(json!({
        "ts": now,
        "purpose": purpose,
        "ms": ms,
        "bytes": bytes,
        "questions": questions.len(),
        "shortened": shortened,
        "ok": result.is_ok(),
        "error": result.as_ref().err(),
        "model": result.as_ref().ok().and_then(|v| v["model"].as_str()),
    }));
    result
}

async fn send(http: &reqwest::Client, url: &str, key: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let resp = http
        .post(url)
        .bearer_auth(key)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "timeout".to_string() } else { e.to_string() })?;
    if !resp.status().is_success() {
        let s = resp.status();
        return Err(format!("{s}: {}", crate::llm::truncate(&resp.text().await.unwrap_or_default(), 200)));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn log(line: Value) {
    use std::io::Write;
    let Ok(p) = Paths::resolve() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p.home.join("jev.log")) {
        let _ = writeln!(f, "{line}");
    }
}

/// Shorten the longest strings in `state` until it fits `budget` bytes, and
/// add a note saying text was omitted. Returns how many strings were cut.
fn fit(state: &mut Value, budget: usize) -> usize {
    let mut cut = 0;
    for _ in 0..64 {
        let size = state.to_string().len();
        if size <= budget {
            break;
        }
        let Some(s) = longest(state) else { break };
        let keep = s.chars().count().saturating_sub(size - budget + 64).max(200) / 2;
        if s.chars().count() <= keep * 2 + 40 {
            break;
        }
        let head: String = s.chars().take(keep).collect();
        let tail: String = s.chars().rev().take(keep).collect::<Vec<_>>().into_iter().rev().collect();
        *s = format!("{head} [... omitted ...] {tail}");
        cut += 1;
    }
    if cut > 0
        && let Some(o) = state.as_object_mut()
    {
        o.insert(
            "evidence_budget".into(),
            json!("Some text was shortened to fit; omitted text is unknown, not evidence of absence."),
        );
    }
    cut
}

fn longest(v: &mut Value) -> Option<&mut String> {
    match v {
        Value::String(s) => Some(s),
        Value::Array(a) => a.iter_mut().filter_map(longest).max_by_key(|s| s.len()),
        Value::Object(o) => o.values_mut().filter_map(longest).max_by_key(|s| s.len()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_shortens_longest_and_marks_it() {
        let mut v = json!({ "request": "short", "errors": ["x".repeat(20_000), "y".repeat(300)] });
        let cut = fit(&mut v, 2_000);
        assert!(cut > 0);
        assert!(v.to_string().len() <= 2_400, "{}", v.to_string().len());
        assert_eq!(v["request"], "short");
        assert!(v["errors"][0].as_str().unwrap().contains("[... omitted ...]"));
        assert!(v["evidence_budget"].is_string());
        let mut small = json!({ "request": "hi" });
        assert_eq!(fit(&mut small, 2_000), 0);
        assert!(small.get("evidence_budget").is_none());
    }
}
