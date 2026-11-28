//! Wake scripts end to end (#417, ADR 0079): a real `construct watch` with a
//! JS filter in `$CONSTRUCT_HOME/scripts/wakes/`. Noisy events are dropped
//! before any model call until the script's persisted count trips; an edit
//! on disk takes effect on the next event without a restart; a broken edit
//! keeps the previous version running and leaves a `note` row in the session
//! the next wake reaches. Assertions read the requests the model got and the
//! journal, never only what was printed.

use crate::common::*;
use crate::wakes::{emit, file, rows, stop, wait_rows, watch};
use serde_json::json;

const CONFIG: &str = "[router]\nmode = \"local\"\n[learn]\nenabled = false\n\
                      [[wakes]]\nname = \"disk\"\nkind = \"cli\"\nsession = \"dedicated\"\n";

const COUNT: &str = r#"
export const wake = { on: ["disk"] };
export async function onEvent(event, ctx) {
  const n = (ctx.state.get("n") ?? 0) + 1;
  ctx.state.set("n", n);
  ctx.log(`seen ${n}`);
  if (n >= 3) {
    ctx.state.delete("n");
    ctx.wake({ summary: `disk noisy ${n} times, last: ${event.summary}` });
  }
}
"#;

const EVERY: &str = r#"
export const wake = { on: ["disk"] };
export function onEvent(event, ctx) { ctx.wake({ summary: `v2 ${event.summary}` }); }
"#;

fn script(env: &Env, src: &str) {
    let dir = env.home.join("scripts").join("wakes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("gate.js"), src).unwrap();
}

#[test]
fn a_script_filters_wakes_at_zero_tokens_and_reloads_without_a_restart() {
    let env = Env::new("wk-script", json!({ "steps": [{ "text": "one" }, { "text": "two" }, { "text": "three" }] }));
    env.config(CONFIG);
    script(&env, COUNT);
    let w = watch(&env);

    // Two noisy events: dropped by the script, no session, no request.
    emit(&env, &["disk", "--summary", "a"]);
    emit(&env, &["disk", "--summary", "b"]);
    // The third trips the count; the model sees the script's summary.
    emit(&env, &["disk", "--summary", "c"]);
    wait_rows(&env, "turn_end", 1);
    assert_eq!(env.requests().len(), 1, "the dropped events cost nothing:\n{}", file(&env, "watch.err"));
    let received = rows(&env, "wake_received");
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].payload["summary"], "disk noisy 3 times, last: c");

    // Edit on disk: the next event runs the new version, same process.
    script(&env, EVERY);
    emit(&env, &["disk", "--summary", "d"]);
    wait_rows(&env, "turn_end", 2);

    // A broken edit: v2 keeps running, and the wake it passes carries a note.
    script(&env, "export const wake = { on: [\"disk\"] }; export function onEvent( {");
    emit(&env, &["disk", "--summary", "e"]);
    wait_rows(&env, "turn_end", 3);
    stop(&env, w);

    let summaries: Vec<String> =
        rows(&env, "wake_received").iter().map(|r| r.payload["summary"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(summaries, ["disk noisy 3 times, last: c", "v2 d", "v2 e"]);
    assert_eq!(env.requests().len(), 3);

    let notes = rows(&env, "note");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(
        (notes[0].payload["class"].as_str(), notes[0].payload["kind"].as_str()),
        (Some("script"), Some("reload_failed"))
    );
    let detail = notes[0].payload["detail"].as_str().unwrap_or("");
    assert!(detail.contains("wakes/gate.js") && detail.contains("keeping the previous version"), "{detail}");
    // The note is in the session the wake reached, before that wake.
    let last = &rows(&env, "wake_received")[2];
    assert_eq!(notes[0].session, last.session);
    assert!(notes[0].seq < last.seq);

    let err = file(&env, "watch.err");
    assert!(err.contains("construct: script loaded wakes/gate.js"), "{err}");
    assert!(err.contains("construct: script wakes/gate.js: seen 2"), "ctx.log reaches stderr: {err}");
}
