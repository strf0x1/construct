//! Core wake system (ADR 0079, #416): plain wake event types, session
//! routing policies, per-session wake inbox, extensible `WakeSource`/
//! `WakeFilter` seams, `WakeBus`, and untrusted payload fencing.
//!
//! The design choice: `wake` knows nothing about `agent`, `journal`, or `acp`.
//! Producers emit `WakeEvent`s into one `WakeBus` that owns filtering,
//! deduplication, burst coalescing, cooldown, TTL expiry, and `<wake>` fencing;
//! `agent::wake` delivers both job completions and wakes through one
//! turn-boundary choke point.

pub mod builtins;
pub mod bus;
pub mod fence;
pub mod socket;
pub mod source;

use crate::event::payload::WakeReceived;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use builtins::{CliSource, CommandSource, TimerSource};
pub use bus::{WakeBatch, WakeBus};
pub use fence::{delivered_payload, escape_wake_close, render, unconsumed};
pub use source::{FilterDecision, MatchFilter, SourceCtx, WakeFilter, WakeSource, WakeSources};

/// Whether a wake event's payload comes from a trusted operator/timer or from
/// external/untrusted data that must be fenced against prompt injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeTrust {
    Trusted,
    Untrusted,
}

impl WakeTrust {
    pub fn is_untrusted(self) -> bool {
        matches!(self, Self::Untrusted)
    }

    pub fn from_untrusted(untrusted: bool) -> Self {
        if untrusted { Self::Untrusted } else { Self::Trusted }
    }
}

/// Which session lane a wake trigger routes into (ADR 0079).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    /// Start a new session per wake batch.
    Fresh,
    /// Reuse/rehydrate one persistent session per named lane (`Head.origin`).
    Dedicated(String),
    /// Route into the instance's interactive/main session.
    Shared,
}

impl SessionPolicy {
    /// Parse a config session policy string (`"fresh"`, `"dedicated"`,
    /// `"dedicated:<lane>"`, `"shared"`). Bare `"dedicated"` uses `trigger_name`.
    pub fn parse(raw: &str, trigger_name: &str) -> Self {
        let s = raw.trim();
        if s == "fresh" {
            Self::Fresh
        } else if s == "shared" {
            Self::Shared
        } else if let Some(lane) = s.strip_prefix("dedicated:") {
            let lane = lane.trim();
            Self::Dedicated(if lane.is_empty() { trigger_name.to_string() } else { lane.to_string() })
        } else {
            Self::Dedicated(trigger_name.to_string())
        }
    }
}

/// One wake event produced by a `WakeSource` and routed by `WakeBus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeEvent {
    pub id: String,
    pub source_kind: String,
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    pub occurred_at: u64,
    pub trust: WakeTrust,
    pub summary: String,
    #[serde(default)]
    pub data: Value,
    /// The configured trigger name that produced or accepted this event.
    pub trigger: String,
    pub session: SessionPolicy,
    /// Operator-authored instruction from `[[wakes]]` config (trusted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Whether this trigger allows background learning after the wake turn.
    #[serde(default)]
    pub learn: bool,
    /// Conditions a filter recovered from on the way here (a script that
    /// failed to reload, ADR 0079), journaled as `Event::Note` rows in the
    /// session this event reaches. Never shown to the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<WakeNote>,
}

/// One `Event::Note` a `WakeFilter` hands to the session an event reaches:
/// a filter runs before any session exists, so its notes ride the next event
/// it passes. The filter owns the `class`/`kind` names and caps `detail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeNote {
    pub class: String,
    pub kind: String,
    pub detail: String,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a unique wake event id (`w-<epoch_ms>-<seq>`).
pub fn next_id() -> String {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("w-{}-{seq}", now_ms())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl WakeEvent {
    pub fn to_received(&self) -> WakeReceived {
        WakeReceived {
            id: self.id.clone(),
            kind: self.source_kind.clone(),
            source: self.source_name.clone(),
            untrusted: self.trust.is_untrusted(),
            summary: self.summary.clone(),
            data: self.data.clone(),
            prompt: self.prompt.clone(),
        }
    }
}

/// Per-session queue of wake events waiting for the next turn boundary.
/// Cheap to clone; every clone shares the same queue.
#[derive(Clone, Default)]
pub struct Inbox {
    items: Arc<Mutex<Vec<Queued>>>,
}

struct Queued {
    event: WakeEvent,
    deferred: bool,
}

impl Inbox {
    pub fn push(&self, events: impl IntoIterator<Item = WakeEvent>) {
        let mut g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        for event in events {
            g.push(Queued { event, deferred: false });
        }
    }

    /// Seed unconsumed, non-expired wake events recovered from a journal.
    /// Deferred until explicitly activated (or woken) so opening history alone
    /// never starts a turn.
    pub fn resume(&self, recovered: Vec<WakeEvent>) {
        if recovered.is_empty() {
            return;
        }
        let mut g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        for event in recovered {
            g.push(Queued { event, deferred: true });
        }
    }

    pub fn activate_recovered(&self) {
        let mut g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        for q in g.iter_mut() {
            q.deferred = false;
        }
    }

    pub fn has_pending(&self) -> bool {
        let g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        g.iter().any(|q| !q.deferred)
    }

    pub fn drain_pending(&self) -> Vec<WakeEvent> {
        let mut g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let mut ready = Vec::new();
        let mut kept = Vec::new();
        for q in g.drain(..) {
            if q.deferred {
                kept.push(q);
            } else {
                ready.push(q.event);
            }
        }
        *g = kept;
        ready
    }
}
