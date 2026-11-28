//! `WakeBus`: centralizes filtering, deduplication, burst coalescing, cooldown
//! suppression, and TTL expiry so no wake source reimplements them (ADR 0079).
//!
//! The design choice: the stateful policy engine (`BusState`) is pure and takes
//! timestamps explicitly, so every coalescing, cooldown, dedup and expiry rule
//! is deterministically unit-tested without wall-clock sleeps.

use super::{FilterDecision, MatchFilter, SessionPolicy, WakeEvent, WakeFilter};
use crate::config::{WakeRuntimeConfig, WakeTriggerConfig};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// One ready-to-deliver batch emitted by `WakeBus` for a single trigger.
#[derive(Debug, Clone)]
pub struct WakeBatch {
    pub trigger: String,
    pub source_kind: String,
    pub source_name: String,
    pub session: SessionPolicy,
    pub learn: bool,
    pub events: Vec<WakeEvent>,
    /// How many events were suppressed by cooldown or dedup since the last batch.
    pub suppressed: u64,
}

#[derive(Clone, Copy)]
struct TriggerPolicy {
    coalesce: Duration,
    cooldown: Duration,
    ttl: Duration,
}

struct Pending {
    flush_at: Instant,
    events: Vec<WakeEvent>,
    dedup: BTreeSet<String>,
}

/// Pure policy state behind `WakeBus`.
struct BusState {
    policies: HashMap<String, TriggerPolicy>,
    default_ttl: Duration,
    pending: HashMap<String, Pending>,
    cooldown_until: HashMap<String, Instant>,
    suppressed: HashMap<String, u64>,
}

impl BusState {
    fn new(runtime: &WakeRuntimeConfig, triggers: &[WakeTriggerConfig]) -> Self {
        let default_ttl = runtime.default_ttl_duration().unwrap_or(Duration::from_secs(900));
        let mut policies = HashMap::new();
        for t in triggers {
            let coalesce = t.coalesce_duration().unwrap_or(Duration::ZERO);
            let cooldown = t.cooldown_duration().unwrap_or(Duration::ZERO);
            let ttl = t.ttl_duration(default_ttl).unwrap_or(default_ttl);
            policies.insert(t.name.clone(), TriggerPolicy { coalesce, cooldown, ttl });
        }
        Self {
            policies,
            default_ttl,
            pending: HashMap::new(),
            cooldown_until: HashMap::new(),
            suppressed: HashMap::new(),
        }
    }

    fn policy_for(&self, trigger: &str) -> TriggerPolicy {
        self.policies.get(trigger).copied().unwrap_or(TriggerPolicy {
            coalesce: Duration::ZERO,
            cooldown: Duration::ZERO,
            ttl: self.default_ttl,
        })
    }

    /// Accept one post-filter event. Returns a batch immediately when no
    /// coalescing window applies and cooldown permits; otherwise buffers or
    /// suppresses it.
    fn push(&mut self, event: WakeEvent, now: Instant, now_epoch_ms: u64) -> Option<WakeBatch> {
        let pol = self.policy_for(&event.trigger);
        if !pol.ttl.is_zero() && now_epoch_ms.saturating_sub(event.occurred_at) > pol.ttl.as_millis() as u64 {
            return None;
        }
        if self.cooldown_until.get(&event.trigger).is_some_and(|&until| now < until) {
            *self.suppressed.entry(event.trigger).or_insert(0) += 1;
            return None;
        }
        if pol.coalesce.is_zero() {
            return Some(self.finalize(event.trigger.clone(), vec![event], now, pol.cooldown));
        }
        let trigger = event.trigger.clone();
        let entry = self.pending.entry(trigger.clone()).or_insert_with(|| Pending {
            flush_at: now + pol.coalesce,
            events: Vec::new(),
            dedup: BTreeSet::new(),
        });
        if let Some(key) = &event.dedup_key
            && !entry.dedup.insert(key.clone())
        {
            *self.suppressed.entry(trigger).or_insert(0) += 1;
            return None;
        }
        entry.events.push(event);
        None
    }

    /// Earliest coalesce flush deadline across all pending triggers.
    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|p| p.flush_at).min()
    }

    /// Flush every pending trigger whose coalesce deadline has arrived.
    fn flush_due(&mut self, now: Instant) -> Vec<WakeBatch> {
        let due: Vec<String> = self.pending.iter().filter(|(_, p)| p.flush_at <= now).map(|(k, _)| k.clone()).collect();
        let mut out = Vec::new();
        for trigger in due {
            if let Some(p) = self.pending.remove(&trigger)
                && !p.events.is_empty()
            {
                let pol = self.policy_for(&trigger);
                out.push(self.finalize(trigger, p.events, now, pol.cooldown));
            }
        }
        out
    }

    fn finalize(&mut self, trigger: String, events: Vec<WakeEvent>, now: Instant, cooldown: Duration) -> WakeBatch {
        if !cooldown.is_zero() {
            self.cooldown_until.insert(trigger.clone(), now + cooldown);
        }
        let suppressed = self.suppressed.remove(&trigger).unwrap_or(0);
        let first = &events[0];
        WakeBatch {
            trigger,
            source_kind: first.source_kind.clone(),
            source_name: first.source_name.clone(),
            session: first.session.clone(),
            learn: events.iter().any(|e| e.learn),
            events,
            suppressed,
        }
    }
}

/// Running wake bus task handle and channels.
pub struct WakeBus {
    pub ingress: mpsc::UnboundedSender<WakeEvent>,
    pub batches: mpsc::UnboundedReceiver<WakeBatch>,
    pub task: JoinHandle<()>,
}

impl WakeBus {
    /// Start a `WakeBus` task that filters, deduplicates, coalesces, and
    /// rate-limits events until `cancel` turns `true` or all senders drop.
    pub fn start(
        runtime: &WakeRuntimeConfig,
        triggers: &[WakeTriggerConfig],
        extra_filters: Vec<Arc<dyn WakeFilter>>,
        mut cancel: watch::Receiver<bool>,
    ) -> Self {
        let mut filters: Vec<Arc<dyn WakeFilter>> = vec![Arc::new(MatchFilter::from_config(triggers))];
        filters.extend(extra_filters);
        let mut state = BusState::new(runtime, triggers);
        let (ingress, mut raw_rx) = mpsc::unbounded_channel::<WakeEvent>();
        let (batch_tx, batches) = mpsc::unbounded_channel::<WakeBatch>();
        let task = tokio::spawn(async move {
            loop {
                let next = state.next_deadline();
                tokio::select! {
                    biased;
                    _ = cancel.changed() => {
                        if *cancel.borrow() {
                            break;
                        }
                    }
                    _ = async {
                        if let Some(at) = next {
                            tokio::time::sleep_until(at).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        for batch in state.flush_due(Instant::now()) {
                            if batch_tx.send(batch).is_err() {
                                return;
                            }
                        }
                    }
                    got = raw_rx.recv() => {
                        let Some(event) = got else { break };
                        let mut current = Some(event);
                        for f in &filters {
                            let Some(ev) = current.take() else { break };
                            if let FilterDecision::Pass(next) = f.filter(ev).await {
                                current = Some(*next);
                            } else {
                                break;
                            }
                        }
                        let Some(event) = current else { continue };
                        if let Some(batch) = state.push(event, Instant::now(), super::now_ms())
                            && batch_tx.send(batch).is_err()
                        {
                            return;
                        }
                    }
                }
            }
        });
        Self { ingress, batches, task }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::WakeTrust;
    use serde_json::json;

    fn ev(id: &str, trigger: &str, dedup: Option<&str>, occurred_at: u64) -> WakeEvent {
        WakeEvent {
            id: id.into(),
            source_kind: "cli".into(),
            source_name: trigger.into(),
            dedup_key: dedup.map(str::to_string),
            occurred_at,
            trust: WakeTrust::Untrusted,
            summary: format!("msg {id}"),
            data: json!({"id": id}),
            trigger: trigger.into(),
            session: SessionPolicy::Dedicated(trigger.into()),
            prompt: None,
            learn: false,
            notes: Vec::new(),
        }
    }

    #[test]
    fn coalesces_bursts_deduplicates_and_enforces_cooldown() {
        let triggers = vec![WakeTriggerConfig {
            name: "burst".into(),
            kind: "cli".into(),
            coalesce: Some("100ms".into()),
            cooldown: Some("500ms".into()),
            ..Default::default()
        }];
        let mut st = BusState::new(&WakeRuntimeConfig::default(), &triggers);
        let t0 = Instant::now();
        assert!(st.push(ev("1", "burst", Some("k1"), 10_000), t0, 10_000).is_none());
        // Duplicate key in same coalesce window is suppressed.
        assert!(st.push(ev("2", "burst", Some("k1"), 10_010), t0 + Duration::from_millis(10), 10_010).is_none());
        assert!(st.push(ev("3", "burst", Some("k2"), 10_020), t0 + Duration::from_millis(20), 10_020).is_none());
        // Before deadline: nothing flushes.
        assert!(st.flush_due(t0 + Duration::from_millis(50)).is_empty());
        // At deadline: one coalesced batch with ids 1 and 3, 1 suppressed.
        let flushed = st.flush_due(t0 + Duration::from_millis(100));
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].events.len(), 2);
        assert_eq!(flushed[0].suppressed, 1);
        // Inside cooldown window: suppressed.
        assert!(st.push(ev("4", "burst", None, 10_200), t0 + Duration::from_millis(200), 10_200).is_none());
        assert!(st.next_deadline().is_none());
        // After cooldown expires: accepted again.
        assert!(st.push(ev("5", "burst", None, 10_700), t0 + Duration::from_millis(700), 10_700).is_none());
        let flushed2 = st.flush_due(t0 + Duration::from_millis(800));
        assert_eq!(flushed2.len(), 1);
        assert_eq!(flushed2[0].events[0].id, "5");
        assert_eq!(flushed2[0].suppressed, 1);
    }

    #[test]
    fn drops_events_past_ttl() {
        let triggers = vec![WakeTriggerConfig {
            name: "t".into(),
            kind: "timer".into(),
            every: Some("1s".into()),
            ttl: Some("1s".into()),
            ..Default::default()
        }];
        let mut st = BusState::new(&WakeRuntimeConfig::default(), &triggers);
        let t0 = Instant::now();
        assert!(st.push(ev("stale", "t", None, 1_000), t0, 5_000).is_none());
        let fresh = st.push(ev("ok", "t", None, 4_500), t0, 5_000);
        assert!(fresh.is_some());
    }
}
