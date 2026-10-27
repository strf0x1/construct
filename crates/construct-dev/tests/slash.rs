//! The slash palette and `/<skill>` from the user's side: what the palette
//! lists as they type, what Enter leaves in the input, and what the model is
//! sent when a skill is run by name.

mod common;

use common::*;
use construct_dev::drive;
use serde_json::{Value, json};

const SKILL_MD: &str = "---\nname: create-verification\ndescription: Build a verification skill that drives the app\n---\n\n1. Find-the-entry-point.\n";

/// The text of the last user message in a request.
fn last_user(req: &Value) -> String {
    req["messages"]
        .as_array()
        .and_then(|m| m.iter().rfind(|m| m["role"] == "user"))
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn run(d: &mut drive::Driver, log: &mut Vec<u8>, script: &str) {
    if let Err(e) = drive::run_script(d, script, log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(log));
    }
}

/// A skill the agent writes in turn 1 is in the palette and runnable in
/// turn 2, with no restart: `/cre` lists it first, Enter completes it, and
/// the request carries its SKILL.md and what the user typed after it.
#[test]
fn a_skill_written_mid_session_runs_from_the_palette() {
    let env = Env::new(
        "slash-skill",
        json!({ "steps": [
            { "call": "skill_manage", "args": { "action": "create", "name": "create-verification", "content": SKILL_MD } },
            { "text": "made-it" },
            { "text": "ran-it" },
            { "text": "ran-again" }
        ] }),
    );
    env.config("[router]\nmode = \"local\"\n[learn]\nenabled = false\n");
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        100,
        30,
    )
    .unwrap();
    let mut log = Vec::new();
    let (short, long) = (budget(3000), budget(5000));
    run(
        &mut d,
        &mut log,
        &format!(
            "wait \"ask construct\" {short}\ntype make a skill for verification\nkey enter\nwait \"made-it\" {long}\n\
             type /cre\nwait \"▸ /create-verification\" {short}\n"
        ),
    );
    let screen = d.text();
    let rows: Vec<&str> = screen.lines().skip_while(|l| !l.starts_with("› /cre")).skip(1).collect();
    assert!(rows[0].contains("▸ /create-verification") && rows[0].contains("Build a verification"), "{screen}");

    run(&mut d, &mut log, &format!("key enter\ngone \"▸ /create-verification\" {short}\n"));
    let screen = d.text();
    assert!(screen.lines().any(|l| l.trim_end() == "› /create-verification"), "enter completes it\n{screen}");

    run(&mut d, &mut log, &format!("type do-the-thing\nkey enter\nwait \"ran-it\" {long}\n"));
    let reqs = env.requests();
    let sent = last_user(reqs.last().unwrap());
    assert!(sent.contains("<skill name=\"create-verification\""), "{sent}");
    assert!(sent.contains("1. Find-the-entry-point."), "the SKILL.md body: {sent}");
    assert!(sent.trim_end().ends_with("do-the-thing"), "the request comes last: {sent}");
    let invoked: Vec<_> = env.journal().into_iter().filter(|r| r.ty == "invoke").collect();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].payload["skill"], "create-verification");
    assert_eq!(invoked[0].payload["scope"], "global");

    // `/skills <name> <request>` runs it too.
    run(
        &mut d,
        &mut log,
        &format!("type /skills create-verification again-please\nkey enter\nwait \"ran-again\" {long}\n"),
    );
    let sent = last_user(env.requests().last().unwrap());
    assert!(sent.contains("1. Find-the-entry-point.") && sent.ends_with("again-please"), "{sent}");
    d.kill();
}

/// Built-in commands run from the palette, esc closes it, and a name that is
/// neither a command nor a skill is still an unknown command.
#[test]
fn the_palette_runs_commands_and_esc_closes_it() {
    let env = Env::new("slash-builtin", json!({ "steps": [ { "text": "unused" } ] }));
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        100,
        30,
    )
    .unwrap();
    let mut log = Vec::new();
    let short = budget(3000);
    run(
        &mut d,
        &mut log,
        &format!(
            "wait \"ask construct\" {short}\ntype /mem\nwait \"▸ /memory\" {short}\nkey enter\nwait \"memory is empty\" {short}\n\
             type /mo\nwait \"▸ /model\" {short}\nkey enter\nwait \"no models in the catalog yet\" {short}\n"
        ),
    );
    assert!(
        d.text().lines().all(|l| l.trim_end() != "› /model"),
        "an optional argument runs, not completes\n{}",
        d.text()
    );
    run(
        &mut d,
        &mut log,
        &format!(
            "key ctrl-u\ntype /he\nwait \"▸ /help\" {short}\nkey esc\ngone \"▸ /help\" {short}\n\
             key ctrl-u\ntype /zzz-nope extra\nkey enter\nwait \"unknown command /zzz-nope\" {short}\n"
        ),
    );
    assert!(env.requests().is_empty(), "no slash command reached the model");
    d.kill();
}

/// After `/model ` the palette lists the endpoint's cached models with their
/// window and price; enter completes the id, a second enter switches to it,
/// and an id the list does not know still works, with no list shown for it.
#[test]
fn model_ids_complete_from_the_cached_list() {
    let env = Env::new("slash-model-ids", json!({ "steps": [ { "text": "unused" } ] }));
    env.config("model = \"mock\"\n[provider]\ndialect = \"openrouter\"\n[learn]\nenabled = false\n");
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        100,
        30,
    )
    .unwrap();
    let mut log = Vec::new();
    let short = budget(3000);
    // The list is fetched at startup; give the agent task a moment to cache it.
    run(
        &mut d,
        &mut log,
        &format!(
            "wait \"ask construct\" {short}\nsleep 500\ntype /model mock/\nwait \"▸ mock/small\" {short}\n\
             expect \"32.8k\"\nkey enter\ngone \"32.8k\" {short}\n"
        ),
    );
    assert!(d.text().lines().any(|l| l.trim_end() == "› /model mock/small"), "enter completes the id\n{}", d.text());
    run(&mut d, &mut log, &format!("key enter\nwait \"model → mock/small\" {short}\nexpect \"32.8k ctx\"\n"));
    run(
        &mut d,
        &mut log,
        &format!(
            "type /model not-in-the-list\ngone \"▸\" {short}\nkey enter\nwait \"model → not-in-the-list\" {short}\n"
        ),
    );
    assert!(env.requests().is_empty(), "no model request was made");
    d.kill();
}
