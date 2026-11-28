//! `WakeScripts` as the `WakeBus` drives it: one event at a time, reloading
//! between firings, state kept across restarts, last-known-good on a broken
//! edit, and trust that a script can raise but never lower.

use construct_core::wake::{FilterDecision, SessionPolicy, WakeEvent, WakeFilter, WakeTrust};
use construct_script::{EVENT_FAILED, Limits, NOTE_CLASS, RELOAD_FAILED, WakeScripts};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Home {
    root: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("cs-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("wakes")).unwrap_or_else(|e| panic!("{e}"));
        Self { root }
    }

    fn write(&self, name: &str, src: &str) {
        std::fs::write(self.root.join("wakes").join(name), src).unwrap_or_else(|e| panic!("{e}"));
    }

    fn scripts(&self) -> WakeScripts {
        WakeScripts::new(self.root.join("wakes"), self.root.join("state.sqlite"), Limits::default())
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn event(trigger: &str, trust: WakeTrust) -> WakeEvent {
    WakeEvent {
        id: construct_core::wake::next_id(),
        source_kind: "cli".into(),
        source_name: trigger.into(),
        dedup_key: None,
        occurred_at: construct_core::wake::now_ms(),
        trust,
        summary: "tick".into(),
        data: json!({"n": 1}),
        trigger: trigger.into(),
        session: SessionPolicy::Dedicated(trigger.into()),
        prompt: None,
        learn: false,
        notes: Vec::new(),
    }
}

async fn pass(s: &WakeScripts, e: WakeEvent) -> Option<WakeEvent> {
    match s.filter(e).await {
        FilterDecision::Pass(e) => Some(*e),
        FilterDecision::Drop => None,
    }
}

const THRESHOLD: &str = r#"
export const wake = { on: ["tick"] };
export async function onEvent(event, ctx) {
  const n = (ctx.state.get("n") ?? 0) + 1;
  ctx.state.set("n", n);
  if (n % 3 === 0) ctx.wake({ summary: `tick x${n}`, data: { n, from: event.trigger } });
}
"#;

#[tokio::test]
async fn wakes_only_past_a_threshold_and_keeps_count_across_restarts() {
    let home = Home::new("threshold");
    home.write("count.js", THRESHOLD);
    let s = home.scripts();
    assert!(pass(&s, event("tick", WakeTrust::Trusted)).await.is_none());
    assert!(pass(&s, event("tick", WakeTrust::Trusted)).await.is_none());
    let woke = pass(&s, event("tick", WakeTrust::Trusted)).await.expect("third tick wakes");
    assert_eq!(woke.summary, "tick x3");
    assert_eq!(woke.data, json!({"n": 3, "from": "tick"}));
    // Triggers the script does not name pass untouched.
    assert_eq!(pass(&s, event("other", WakeTrust::Trusted)).await.map(|e| e.summary).as_deref(), Some("tick"));

    // A new process (a fresh filter) picks the count up from the store.
    let s = home.scripts();
    assert!(pass(&s, event("tick", WakeTrust::Trusted)).await.is_none());
    assert!(pass(&s, event("tick", WakeTrust::Trusted)).await.is_none());
    assert_eq!(pass(&s, event("tick", WakeTrust::Trusted)).await.map(|e| e.summary).as_deref(), Some("tick x6"));
}

#[tokio::test]
async fn a_broken_edit_keeps_the_last_good_version_and_leaves_a_note() {
    let home = Home::new("lkg");
    home.write(
        "gate.js",
        r#"export const wake = { on: "tick" }; export function onEvent(e, ctx) { ctx.wake({ summary: "v1" }); }"#,
    );
    let s = home.scripts();
    assert_eq!(pass(&s, event("tick", WakeTrust::Trusted)).await.map(|e| e.summary).as_deref(), Some("v1"));

    home.write("gate.js", "export const wake = { on: [\"tick\"] }; export function onEvent( {");
    let woke = pass(&s, event("tick", WakeTrust::Trusted)).await.expect("v1 still runs");
    assert_eq!(woke.summary, "v1");
    assert_eq!(woke.notes.len(), 1);
    assert_eq!(woke.notes[0].class, NOTE_CLASS);
    assert_eq!(woke.notes[0].kind, RELOAD_FAILED);
    assert!(woke.notes[0].detail.contains("keeping the previous version"), "{}", woke.notes[0].detail);
    // Reported once, not on every firing.
    assert!(pass(&s, event("tick", WakeTrust::Trusted)).await.is_some_and(|e| e.notes.is_empty()));

    home.write(
        "gate.js",
        r#"export const wake = { on: ["tick"] }; export function onEvent(e, ctx) { ctx.wake({ summary: "v2" }); }"#,
    );
    assert_eq!(pass(&s, event("tick", WakeTrust::Trusted)).await.map(|e| e.summary).as_deref(), Some("v2"));
}

#[tokio::test]
async fn a_throw_drops_the_event_and_writes_no_state() {
    let home = Home::new("throw");
    home.write(
        "flaky.js",
        r#"export const wake = { on: ["tick"] };
           export function onEvent(e, ctx) {
             const n = (ctx.state.get("n") ?? 0) + 1;
             ctx.state.set("n", n);
             ctx.wake({ summary: `n=${n}` });
             if (e.data.boom) throw new Error("boom");
           }"#,
    );
    let s = home.scripts();
    let mut boom = event("tick", WakeTrust::Trusted);
    boom.data = json!({"boom": true});
    assert!(pass(&s, boom).await.is_none());
    let next = pass(&s, event("tick", WakeTrust::Trusted)).await.expect("passes");
    assert_eq!(next.summary, "n=1", "the throwing call's write was rolled back");
    assert_eq!(next.notes.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(), vec![EVENT_FAILED]);
}

#[tokio::test]
async fn a_wake_can_raise_trust_and_pick_a_lane_but_never_lower_trust() {
    let home = Home::new("trust");
    home.write(
        "lane.js",
        r#"export const wake = { on: "*" };
           export function onEvent(e, ctx) { ctx.wake({ untrusted: e.data.raise === true ? true : false, session: "fresh" }); }"#,
    );
    let s = home.scripts();
    let woke = pass(&s, event("a", WakeTrust::Untrusted)).await.expect("passes");
    assert_eq!(woke.trust, WakeTrust::Untrusted, "untrusted input stays untrusted");
    assert_eq!(woke.session, SessionPolicy::Fresh);
    let mut raise = event("b", WakeTrust::Trusted);
    raise.data = json!({"raise": true});
    assert_eq!(pass(&s, raise).await.map(|e| e.trust), Some(WakeTrust::Untrusted));
}

#[tokio::test]
async fn a_script_without_its_exports_does_not_load() {
    let home = Home::new("exports");
    home.write("noon.js", "export function onEvent(e, ctx) {}");
    home.write("nofn.js", r#"export const wake = { on: ["tick"] };"#);
    let s = home.scripts();
    // Neither loads, so nothing filters the event.
    assert!(pass(&s, event("tick", WakeTrust::Trusted)).await.is_some());
    assert!(!Path::new(&home.root.join("state.sqlite")).exists(), "no script ran, so no store");
}
