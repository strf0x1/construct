//! Wake scripts: JS filters over `[[wakes]]` triggers that run before any
//! model call (ADR 0079, #417). A script in `$CONSTRUCT_HOME/scripts/wakes/`
//! exports which triggers it handles and an `onEvent` function:
//!
//! ```js
//! export const wake = { on: ["disk"] };          // trigger names, or "*"
//! export async function onEvent(event, ctx) {
//!   const n = (ctx.state.get("n") ?? 0) + 1;
//!   ctx.state.set("n", n);
//!   if (n >= 3) ctx.wake({ summary: `disk noisy ${n} times` });
//! }
//! ```
//!
//! An event a script handles reaches a session only if `onEvent` calls
//! `ctx.wake`; otherwise it is dropped at zero tokens. The design choices: a
//! script that throws or runs out of time drops the event (it was put there
//! to keep noise out), and a wake can make an event untrusted but never
//! trusted. Only the operator's home directory is read: a project's
//! `.construct/scripts/` is writable by the agent, and agent-proposed scripts
//! wait for the approval flow (#421).

use crate::bridge::{self, Call};
use crate::reload::{Change, Library};
use crate::sandbox::{self, Compiled, Limits};
use crate::state::{Store, Txn};
use async_trait::async_trait;
use construct_core::config::Paths;
use construct_core::wake::{FilterDecision, SessionPolicy, WakeEvent, WakeFilter, WakeNote, WakeTrust};
use rquickjs::{Function, Object};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// The `class` of the notes a wake script leaves in a session's journal.
pub const NOTE_CLASS: &str = "script";
/// A new version of a script failed to load (the previous one, if any, runs on).
pub const RELOAD_FAILED: &str = "reload_failed";
/// A script threw or broke a limit while handling an event (it was dropped).
pub const EVENT_FAILED: &str = "event_failed";
/// The longest note detail, in bytes.
const NOTE_MAX: usize = 300;

/// The `WakeFilter` the binary's `script` feature registers.
#[derive(Clone)]
pub struct WakeScripts {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    library: Library<WakeScript>,
    state_file: PathBuf,
    store: Option<Arc<Store>>,
    limits: Limits,
    /// Notes per script, waiting for the next event that script passes.
    notes: BTreeMap<String, Vec<WakeNote>>,
}

struct WakeScript {
    compiled: Compiled,
    on: Vec<String>,
}

impl WakeScript {
    fn handles(&self, trigger: &str) -> bool {
        self.on.iter().any(|t| t == "*" || t == trigger)
    }
}

impl WakeScripts {
    /// Scripts under this home's `scripts/wakes/`, state in `scripts/state.sqlite`.
    pub fn for_home(paths: &Paths) -> Self {
        let root = paths.home.join("scripts");
        Self::new(root.join("wakes"), root.join("state.sqlite"), Limits::default())
    }

    pub fn new(dir: PathBuf, state_file: PathBuf, limits: Limits) -> Self {
        let inner = Inner { library: Library::new(dir), state_file, store: None, limits, notes: BTreeMap::new() };
        Self { inner: Arc::new(Mutex::new(inner)) }
    }
}

#[async_trait]
impl WakeFilter for WakeScripts {
    async fn filter(&self, event: WakeEvent) -> FilterDecision {
        let inner = self.inner.clone();
        let fallback = event.clone();
        let run = tokio::task::spawn_blocking(move || {
            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
            g.filter(event)
        });
        // A panic in the filter is a bug here, not a reason to lose the event.
        run.await.unwrap_or_else(|_| FilterDecision::pass(fallback))
    }
}

impl Inner {
    fn filter(&mut self, event: WakeEvent) -> FilterDecision {
        // Between firings is the boundary: reload before this event, never during.
        let limits = self.limits;
        for change in self.library.refresh(|name, src| load(name, src, &limits)) {
            self.report(change);
        }
        let mut current = event;
        let mut handled = Vec::new();
        let scripts: Vec<(String, Arc<WakeScript>)> = self
            .library
            .iter()
            .filter(|(_, s)| s.handles(&current.trigger))
            .map(|(n, s)| (n.into(), s.clone()))
            .collect();
        for (name, script) in scripts {
            match self.call(&name, &script, &current) {
                Ok(Some(next)) => current = next,
                Ok(None) => return FilterDecision::Drop,
                Err(e) => {
                    self.note(&name, EVENT_FAILED, format!("wakes/{name}.js dropped {:?}: {e}", current.trigger));
                    return FilterDecision::Drop;
                }
            }
            handled.push(name);
        }
        for name in handled {
            current.notes.extend(self.notes.remove(&name).unwrap_or_default());
        }
        FilterDecision::pass(current)
    }

    fn report(&mut self, change: Change) {
        match change {
            Change::Loaded { name } => status(&format!("loaded wakes/{name}.js")),
            Change::Removed { name } => status(&format!("removed wakes/{name}.js")),
            Change::Failed { name, error, kept } => {
                let tail = if kept { "keeping the previous version" } else { "not loaded" };
                self.note(&name, RELOAD_FAILED, format!("wakes/{name}.js: {error}; {tail}"));
            }
        }
    }

    fn note(&mut self, script: &str, kind: &str, detail: String) {
        status(&detail);
        let detail = bridge::cap(&detail, NOTE_MAX);
        let list = self.notes.entry(script.to_string()).or_default();
        // A script failing on every tick leaves one note per kind until it passes.
        if !list.iter().any(|n| n.kind == kind) {
            list.push(WakeNote { class: NOTE_CLASS.into(), kind: kind.into(), detail });
        }
    }

    fn store(&mut self) -> Result<Arc<Store>, String> {
        if let Some(s) = &self.store {
            return Ok(s.clone());
        }
        let s = Store::open(&self.state_file)?;
        self.store = Some(s.clone());
        Ok(s)
    }

    /// Run one script on one event: `Some` is the event to pass on.
    fn call(&mut self, name: &str, script: &WakeScript, event: &WakeEvent) -> Result<Option<WakeEvent>, String> {
        let txn = self.store().map(|s| Txn::new(s, name));
        let call = Call::new(txn, self.limits.output);
        let arg = event_json(event).to_string();
        sandbox::run(&script.compiled, &self.limits, |ctx, ns| {
            let on_event: Function = ns.get("onEvent")?;
            let ev = ctx.json_parse(arg)?;
            let c = bridge::wake_ctx(ctx, &call)?;
            sandbox::settle(on_event.call((ev, c))?)?;
            Ok(())
        })?;
        let call = Rc::try_unwrap(call).map_err(|_| "the script kept its ctx".to_string())?.into_inner();
        for line in &call.logs {
            status(&format!("wakes/{name}.js: {line}"));
        }
        // An unopenable store only matters to a script that used it, and that one threw.
        if let Ok(txn) = call.state {
            txn.commit()?;
        }
        Ok(call.wake.map(|w| woken(event, &w)))
    }
}

/// Compile and dry-run a script: it must parse, export `onEvent`, and name
/// the triggers it handles in `wake.on`.
fn load(name: &str, source: &str, limits: &Limits) -> Result<WakeScript, String> {
    let compiled = sandbox::compile(&format!("wakes/{name}.js"), source)?;
    let on = sandbox::run(&compiled, limits, |ctx, ns| {
        let f: rquickjs::Value = ns.get("onEvent")?;
        if !f.is_function() {
            return Err(rquickjs::Exception::throw_message(ctx, "must export a function onEvent(event, ctx)"));
        }
        let meta: Option<Object> = ns.get("wake")?;
        let on: rquickjs::Value = match meta {
            Some(m) => m.get("on")?,
            None => rquickjs::Value::new_undefined(ctx.clone()),
        };
        let json = ctx.json_stringify(on)?.map(|s| s.to_string()).transpose()?;
        Ok(json)
    })?;
    let on = match on.as_deref().map(serde_json::from_str::<Value>) {
        Some(Ok(Value::String(s))) => vec![s],
        Some(Ok(Value::Array(a))) => a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    if on.is_empty() {
        return Err("must export const wake = { on: [\"<trigger>\", ...] }".into());
    }
    Ok(WakeScript { compiled, on })
}

/// What `onEvent` sees as its `event`.
fn event_json(e: &WakeEvent) -> Value {
    json!({
        "id": e.id,
        "trigger": e.trigger,
        "kind": e.source_kind,
        "source": e.source_name,
        "summary": e.summary,
        "data": e.data,
        "occurredAt": e.occurred_at,
        "untrusted": e.trust.is_untrusted(),
    })
}

/// The event a `ctx.wake({ summary?, data?, session?, untrusted? })` passes on.
fn woken(e: &WakeEvent, w: &Value) -> WakeEvent {
    let mut out = e.clone();
    if let Some(s) = w.get("summary").and_then(Value::as_str) {
        out.summary = s.to_string();
    }
    if let Some(d) = w.get("data") {
        out.data = d.clone();
    }
    if let Some(s) = w.get("session").and_then(Value::as_str) {
        out.session = SessionPolicy::parse(s, &e.trigger);
    }
    let untrusted = w.get("untrusted").and_then(Value::as_bool).unwrap_or(false);
    out.trust = WakeTrust::from_untrusted(e.trust.is_untrusted() || untrusted);
    out
}

fn status(line: &str) {
    eprintln!("construct: script {line}");
}
