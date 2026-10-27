//! The model catalog from the user's side: `/model` lists the hand-written
//! entries and switching changes the next request and the status line, and a
//! catalog entry's window is the one compaction plans against.

mod common;

use common::*;
use construct_dev::drive;
use serde_json::{Value, json};

const MODELS: &str = r#"
[models."a/one"]
label = "One — fast"
context_window = 200000
max_tokens = 16000

[models."b/two"]
label = "Two — careful"
context_window = 1000000
max_tokens = 32000

[models."tiny/16k"]
label = "Tiny"
context_window = 16384
max_tokens = 1024
"#;

fn write_models(env: &Env, toml: &str) {
    std::fs::create_dir_all(&env.home).unwrap();
    std::fs::write(env.home.join("models.toml"), toml).unwrap();
}

fn run(d: &mut drive::Driver, log: &mut Vec<u8>, script: &str) {
    if let Err(e) = drive::run_script(d, script, log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(log));
    }
}

fn spawn(env: &Env) -> drive::Driver {
    drive::Driver::spawn(&[construct().display().to_string()], &env.env(), Some(env.work.to_str().unwrap()), 120, 34)
        .unwrap()
}

fn last_line(screen: &str) -> String {
    screen.lines().rfind(|l| !l.trim().is_empty()).unwrap_or_default().to_string()
}

/// `/model` opens the picker with every entry and the running one ticked;
/// choosing another changes the status line and the next request's `model`,
/// and the journal says what the switch resolved to.
#[test]
fn model_picker_lists_the_catalog_and_a_choice_reaches_the_next_request() {
    let env = Env::new("catalog-picker", json!({ "steps": [ { "text": "first-reply" }, { "text": "second-reply" } ] }));
    env.config("model = \"a/one\"\n[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    write_models(&env, MODELS);
    let mut d = spawn(&env);
    let mut log = Vec::new();
    let (short, long) = (budget(3000), budget(5000));
    run(
        &mut d,
        &mut log,
        &format!("wait \"ask construct\" {short}\ntype /model\nkey enter\nwait \"search models…\" {short}\n"),
    );
    let screen = d.text();
    for want in ["a/one", "b/two", "tiny/16k", "One — fast", "200k", "1M", "16.4k", "✓"] {
        assert!(screen.contains(want), "picker lacks {want:?}\n{screen}");
    }
    assert!(screen.contains("a/one") && last_line(&screen).contains("a/one"), "status line names the running model");

    // Esc closes it and changes nothing.
    run(&mut d, &mut log, &format!("key esc\ngone \"search models…\" {short}\n"));
    assert!(last_line(&d.text()).contains("a/one"));

    run(
        &mut d,
        &mut log,
        &format!(
            "type /model\nkey enter\nwait \"search models…\" {short}\ntype two\nkey enter\nwait \"model → b/two\" {short}\n"
        ),
    );
    let screen = d.text();
    assert!(screen.contains("model → b/two (Two — careful) · 1M ctx"), "{screen}");
    assert!(last_line(&screen).contains("b/two"), "status line follows: {}", last_line(&screen));

    run(&mut d, &mut log, &format!("type hello\nkey enter\nwait \"first-reply\" {long}\n"));
    let reqs = env.requests();
    assert_eq!(reqs.last().unwrap()["model"], "b/two", "the next request carries the chosen id");
    let rows = env.journal();
    let head = rows.iter().find(|r| r.ty == "head").unwrap();
    assert_eq!(head.payload["manifest"]["context_window"], 200_000, "the head row holds what the session started with");
    assert_eq!(head.payload["manifest"]["max_tokens"], 16_000);
    let sw: Vec<_> = rows.iter().filter(|r| r.ty == "model").collect();
    assert_eq!(sw.len(), 1);
    assert_eq!(sw[0].payload["model"], "b/two");
    assert_eq!(sw[0].payload["context_window"], 1_000_000);
    assert_eq!(sw[0].payload["max_tokens"], 32_000);
    d.kill();
}

/// `/model <id>` still takes any id, and an id with no entry runs on the
/// top-level defaults; with no catalog at all, `/model` says how to make one.
#[test]
fn model_by_id_takes_any_id_and_an_empty_catalog_explains_itself() {
    let env = Env::new("catalog-by-id", json!({ "steps": [ { "text": "ok" } ] }));
    env.config("context_window = 50000\n[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    let mut d = spawn(&env);
    let mut log = Vec::new();
    let short = budget(3000);
    run(
        &mut d,
        &mut log,
        &format!(
            "wait \"ask construct\" {short}\ntype /model\nkey enter\nwait \"no models in the catalog yet\" {short}\n"
        ),
    );
    assert!(d.text().contains("models.toml"), "{}", d.text());
    run(
        &mut d,
        &mut log,
        &format!("type /model some/other-id\nkey enter\nwait \"model → some/other-id · 50k ctx\" {short}\n"),
    );
    assert!(last_line(&d.text()).contains("some/other-id"));
    d.kill();
}

/// `n` rounds that each read a ~8 KB file, then an answer.
fn reads(n: usize) -> Value {
    let mut steps: Vec<Value> =
        (0..n).map(|i| json!({ "call": "read", "args": { "path": format!("big{}.txt", i % 3) } })).collect();
    steps.push(json!({ "text": "done reading" }));
    json!({ "steps": steps })
}

fn big_files(env: &Env) {
    for i in 0..3 {
        let body: String = (0..200).map(|l| format!("file {i} line {l:03}: some ordinary content here\n")).collect();
        std::fs::write(env.work.join(format!("big{i}.txt")), body).unwrap();
    }
}

fn compactions(env: &Env) -> Vec<Value> {
    env.journal().into_iter().filter(|r| r.ty == "compacted").map(|r| r.payload).collect()
}

fn head_window(env: &Env) -> u64 {
    let rows = env.journal();
    rows.iter().find(|r| r.ty == "head").unwrap().payload["manifest"]["context_window"].as_u64().unwrap()
}

/// The global window is 128k; the catalog says this model has 16k, and the
/// endpoint enforces 16k. Compaction has to plan against the entry's window,
/// so no request overflows. The same run on an id with no entry stays whole.
#[test]
fn compaction_plans_against_the_catalog_entrys_window() {
    let env = Env::with_window("catalog-compact", reads(12), Some(16_384));
    env.config("[router]\nmode = \"local\"\n");
    write_models(&env, MODELS);
    big_files(&env);
    let out = env.run(&["--model", "tiny/16k", "run", "--no-learn", "read the big files"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("done reading"));
    let log = std::fs::read_to_string(env.dir.join("log.jsonl")).unwrap();
    assert!(log.lines().all(|l| !l.contains("\"window_rejected\":true")), "no request overflowed the 16k window");
    let cuts = compactions(&env);
    assert!(!cuts.is_empty() && cuts.iter().all(|c| c["reason"] == "threshold"), "{cuts:?}");
    assert_eq!(head_window(&env), 16_384, "the manifest records the resolved window");

    let control = Env::new("catalog-compact-control", reads(12));
    control.config("[router]\nmode = \"local\"\n");
    write_models(&control, MODELS);
    big_files(&control);
    let out = control.run(&["--model", "unlisted/model", "run", "--no-learn", "read the big files"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(compactions(&control).is_empty(), "an id with no entry plans against the 131k default");
    assert_eq!(head_window(&control), 131_072);
}
