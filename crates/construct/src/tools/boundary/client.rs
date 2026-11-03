//! The Deck boundary status endpoint on guest loopback (ADR 0004 §6):
//! read events, long-poll one approval, withdraw one. Nothing here can allow
//! anything; withdraw only removes a pending item.
//!
//! The design choice: one shared handle per session. Tools read it from
//! `Ctx`, the ACP reader pushes late decisions into it, and the agent asks it
//! what happened while a tool ran. Each call is bounded, so a dead endpoint
//! never turns a tool into a hang.

use super::note;
use super::{MAX_QUEUE, Notice};
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The host caps a wait at 300 s; a tool may ask for less.
pub const MAX_WAIT: u64 = 300;
/// An events fetch is loopback and tiny; a slow one is a dead one.
const EVENTS_TIMEOUT: Duration = Duration::from_secs(2);
const WITHDRAW_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub approval_id: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default, alias = "outcome")]
    pub state: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl Entry {
    pub fn state(&self) -> &str {
        self.state.as_deref().unwrap_or("pending")
    }
    /// A decision, as opposed to an item still waiting on the user.
    pub fn terminal(&self) -> bool {
        matches!(self.state(), "allowed" | "denied" | "expired" | "withdrawn")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Outcome {
    #[serde(default, alias = "state")]
    pub outcome: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
}

impl Outcome {
    pub fn terminal(&self) -> bool {
        matches!(self.outcome.as_str(), "allowed" | "denied" | "expired" | "withdrawn")
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum EventsBody {
    Items(Vec<Entry>),
    Named { events: Vec<Entry> },
}

pub struct Boundary {
    base: String,
    http: reqwest::Client,
    /// The highest gateway sequence the agent has read; `None` until the
    /// first probe.
    seq: Mutex<Option<u64>>,
    /// Ids whose terminal state the agent already saw through a tool or note.
    seen: Mutex<HashSet<String>>,
    /// A failed transport: the per-task endpoint is gone, so tools must not
    /// pay its timeout again.
    down: AtomicBool,
    /// Late decisions pushed over ACP, not yet on a model request.
    queued: Mutex<VecDeque<Notice>>,
}

impl Boundary {
    pub fn new(base_url: &str) -> Self {
        #[expect(
            clippy::expect_used,
            reason = "fails only when the TLS backend can't start, and the endpoint is loopback HTTP"
        )]
        let http = reqwest::Client::builder()
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(1))
            .build()
            .expect("http client");
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http,
            seq: Mutex::new(None),
            seen: Mutex::new(HashSet::new()),
            down: AtomicBool::new(false),
            queued: Mutex::new(VecDeque::new()),
        }
    }

    /// The sequence already probed, without touching the endpoint.
    fn current(&self) -> Option<u64> {
        *lock(&self.seq)
    }

    /// The current sequence, probing once so events from before this session
    /// are not attributed to its first tool.
    pub async fn cursor(&self) -> Option<u64> {
        if self.down() {
            return None;
        }
        if let Some(seq) = self.current() {
            return Some(seq);
        }
        let events = self.events(0).await.ok()?;
        let max = events.iter().map(|e| e.seq).max().unwrap_or(0);
        self.advance(max);
        Some(max)
    }

    /// What was blocked after `after`, as the note the model reads. Marks a
    /// terminal state seen so a later `_deck/boundary` does not repeat it.
    pub async fn tool_note(&self, after: u64) -> Option<String> {
        if self.down() {
            return None;
        }
        let events = self.events(after).await.ok()?;
        if events.is_empty() {
            return None;
        }
        if let Some(max) = events.iter().map(|e| e.seq).max() {
            self.advance(max);
        }
        let note = note::tool(&events, |id| self.seen(id));
        for e in events.iter().filter(|e| e.terminal()) {
            self.mark_seen(&e.approval_id);
        }
        note
    }

    /// One approval's state now, or after `wait` seconds. The host caps the
    /// wait, so a larger request is clamped before it leaves the guest.
    pub async fn approval(&self, id: &str, wait: Option<u64>) -> Result<Outcome, String> {
        check_id(id)?;
        if self.down() {
            return Err("the deck boundary endpoint is unreachable".into());
        }
        let wait = wait.unwrap_or(0).min(MAX_WAIT);
        let url = format!("{}/approvals/{}?wait={wait}", self.base, id);
        let r = match self.http.get(url).timeout(Duration::from_secs(wait + 2)).send().await {
            Ok(r) => r,
            Err(e) => {
                self.mark_down();
                return Err(format!("boundary endpoint: {e}"));
            }
        };
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("boundary approval returned {status}: {}", crate::llm::truncate(&body, 200)));
        }
        serde_json::from_str(&body).map_err(|e| format!("boundary approval was not JSON: {e}"))
    }

    /// Drop a pending item. The audit names the agent; nothing is granted.
    pub async fn withdraw(&self, id: &str) -> Result<(), String> {
        check_id(id)?;
        if self.down() {
            return Err("the deck boundary endpoint is unreachable".into());
        }
        let url = format!("{}/approvals/{}/withdraw", self.base, id);
        let r = match self.http.post(url).timeout(WITHDRAW_TIMEOUT).send().await {
            Ok(r) => r,
            Err(e) => {
                self.mark_down();
                return Err(format!("boundary endpoint: {e}"));
            }
        };
        if !r.status().is_success() {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            return Err(format!("boundary withdraw returned {status}: {}", crate::llm::truncate(&body, 200)));
        }
        Ok(())
    }

    fn down(&self) -> bool {
        self.down.load(Ordering::Relaxed)
    }

    fn mark_down(&self) {
        self.down.store(true, Ordering::Relaxed);
    }

    fn advance(&self, max: u64) {
        let mut seq = lock(&self.seq);
        *seq = Some(seq.unwrap_or(0).max(max));
    }

    pub fn mark_seen(&self, id: &str) {
        lock(&self.seen).insert(id.to_string());
    }

    pub fn seen(&self, id: &str) -> bool {
        lock(&self.seen).contains(id)
    }

    /// Queue a late decision; an id already resolved is dropped, and a repeat
    /// replaces the earlier outcome.
    pub fn push_notice(&self, notice: Notice) {
        if self.seen(&notice.id) {
            return;
        }
        let mut queue = lock(&self.queued);
        if let Some(existing) = queue.iter_mut().find(|n| n.id == notice.id) {
            *existing = notice;
            return;
        }
        if queue.len() >= MAX_QUEUE {
            queue.pop_front();
        }
        queue.push_back(notice);
    }

    /// Take what is ready for the next model request, skipping ids resolved
    /// since they were queued.
    pub fn take_notices(&self) -> Vec<Notice> {
        let mut queue = lock(&self.queued);
        let mut out = Vec::new();
        while let Some(n) = queue.pop_front() {
            if !self.seen(&n.id) {
                out.push(n);
            }
        }
        out
    }

    async fn events(&self, after: u64) -> Result<Vec<Entry>, String> {
        let url = format!("{}/events?after={after}", self.base);
        let r = match self.http.get(url).timeout(EVENTS_TIMEOUT).send().await {
            Ok(r) => r,
            Err(e) => {
                self.mark_down();
                return Err(format!("boundary endpoint: {e}"));
            }
        };
        if !r.status().is_success() {
            return Err(format!("boundary events returned {}", r.status()));
        }
        let body: EventsBody = match r.json().await {
            Ok(b) => b,
            Err(e) => {
                self.mark_down();
                return Err(format!("boundary events were not JSON: {e}"));
            }
        };
        Ok(match body {
            EventsBody::Items(v) | EventsBody::Named { events: v } => v,
        })
    }
}

/// Approval ids are opaque hex-ish strings; keep anything that could change
/// the request path out of the URL.
pub fn check_id(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        Ok(())
    } else {
        Err(format!("{id:?} is not an approval id"))
    }
}

#[expect(clippy::expect_used, reason = "poisoned lock: a panic elsewhere")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().expect("poisoned lock: a panic elsewhere")
}
