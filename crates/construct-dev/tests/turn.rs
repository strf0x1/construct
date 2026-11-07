//! A turn's life as the TUI shows it: a message typed while one runs waits its
//! turn, and a `/skill` the worker answers without a turn ends the turn (the
//! `refused` signal) instead of leaving the screen busy forever.

use crate::common::*;
use construct_dev::drive;
use serde_json::json;

fn play(d: &mut drive::Driver, script: &str) {
    let mut log = vec![];
    if let Err(e) = drive::run_script(d, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
}

fn tui(env: &Env, cols: u16, rows: u16) -> drive::Driver {
    drive::Driver::spawn(&[construct().display().to_string()], &env.env(), Some(env.work.to_str().unwrap()), cols, rows)
        .unwrap()
}

/// A message typed while a turn runs is queued: both turns run, in order, and
/// the TUI is idle afterwards rather than stuck on the queue.
#[test]
fn a_message_typed_during_a_turn_waits_for_it_and_leaves_the_tui_idle() {
    let env = Env::new(
        "turn-queued",
        json!({"steps": [
            {"call":"shell","args":{"command":"sleep 2"}},
            {"text":"answer one"},
            {"text":"answer two"}
        ]}),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut d = tui(&env, 100, 30);
    play(
        &mut d,
        "wait \"ask construct\" 5000\ntype first\nkey enter\nwait \"running shell\" 5000\n\
         wait \"queue a message… (esc to interrupt)\" 3000\ntype second\nkey enter\n\
         wait \"answer one\" 10000\nwait \"answer two\" 10000\n\
         wait \"ask construct anything… (/help)\" 5000\ngone \"queue a message\" 2000\n",
    );

    let rows = env.journal();
    let count = |ty: &str| rows.iter().filter(|r| r.ty == ty).count();
    assert_eq!(count("turn_start"), 2, "the queued message did not run as its own turn");
    assert_eq!(count("turn_end"), 2);
    assert_eq!(count("user"), 2);
    let reqs = env.requests();
    assert_eq!(reqs.len(), 3, "expected the tool call, then one request per turn");
    assert!(reqs.last().unwrap().to_string().contains("second"), "the queued message never reached the model");

    let screen = d.text();
    assert!(screen.contains("ask construct anything… (/help)"), "the TUI is not idle after both turns:\n{screen}");
    assert!(!screen.contains("queue a message"), "the TUI is still busy after both turns:\n{screen}");
}

/// A `/skill` the palette accepted, whose SKILL.md is deleted by the turn still
/// running, emits an error and no turn; the refusal settles the queue instead
/// of leaving the TUI busy forever (#189).
#[test]
fn a_skill_that_vanishes_before_it_runs_ends_the_turn() {
    let env = Env::new(
        "turn-refused",
        json!({"steps": [
            // Delayed, so the `/gone` line is typed (and accepted from the
            // library on disk) before this turn deletes the skill.
            {"call":"shell","args":{"command":"rm -f skills/gone/SKILL.md && echo removed"},"delay_ms":800},
            {"text":"done removing"}
        ]}),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    // The global library is `$CONSTRUCT_HOME/skills` and the shell may write
    // only the work dir, so the library is a link the running turn can delete
    // through: no polling thread, no timing bet.
    std::fs::create_dir_all(env.work.join("skills/gone")).unwrap();
    let skill = env.work.join("skills/gone/SKILL.md");
    std::fs::write(&skill, "---\nname: gone\ndescription: A procedure that vanishes\n---\n\n1. Do it.\n").unwrap();
    std::fs::create_dir_all(&env.home).unwrap();
    std::os::unix::fs::symlink(env.work.join("skills"), env.home.join("skills")).unwrap();

    let mut d = tui(&env, 100, 30);
    play(
        &mut d,
        "wait \"ask construct\" 5000\ntype remove it now\nkey enter\ntype /gone go\nkey enter\n\
         wait \"no skill named\" 10000\nwait \"ask construct anything… (/help)\" 5000\ngone \"queue a message\" 2000\n",
    );
    assert!(!skill.exists(), "the running turn did not delete SKILL.md");

    let rows = env.journal();
    let count = |ty: &str| rows.iter().filter(|r| r.ty == ty).count();
    assert_eq!(count("turn_start"), 1, "a turn ran for the refused skill");
    assert_eq!(count("turn_end"), 1);
    assert_eq!(count("invoke"), 0, "the vanished skill was journaled as invoked");
    let error = rows.iter().find(|r| r.ty == "error").expect("no error row for the vanished skill");
    assert!(error.payload.to_string().contains("no skill named"), "wrong error: {}", error.payload);
    let end = rows.iter().find(|r| r.ty == "turn_end").unwrap();
    assert!(error.seq > end.seq, "the refusal came before the turn it did not run");
    assert_eq!(env.requests().len(), 2, "the refused skill sent a model request");

    let screen = d.text();
    assert!(screen.contains("ask construct anything… (/help)"), "the TUI is not idle after the refusal:\n{screen}");
    assert!(!screen.contains("queue a message"), "the TUI stayed busy after the refusal:\n{screen}");
}

/// ctrl-c pressed as the reply ends arms quit rather than interrupting a turn
/// that has already finished. The reply is drawn before `TurnEnd` is folded
/// in, so the key must see the events that already arrived; on a loaded runner
/// the key used to win that race and the toast never appeared.
#[test]
fn a_quit_pressed_as_the_reply_ends_arms_instead_of_interrupting() {
    let env = Env::new("turn-quit-gap", json!({"steps":[{"text":"done"}]}));
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut d = tui(&env, 100, 30);
    play(&mut d, "wait \"ask construct\" 5000\ntype hi\nkey enter\nwait \"done\" 5000\nkey ctrl-c\nwait \"press ctrl+c again to quit\" 3000\n");
    let rows = env.journal();
    let ends: Vec<_> = rows.iter().filter(|r| r.ty == "turn_end").collect();
    assert_eq!(ends.len(), 1, "{rows:?}");
    assert_eq!(ends[0].payload["interrupted"], json!(false), "the ctrl-c interrupted a finished turn: {rows:?}");
    assert!(d.text().contains("press ctrl+c again to quit"));
}
