//! Real forced termination at durable boundaries, followed by a fresh process.
//! The next request must be protocol-valid, idempotent and free of tool retries.

use crate::common::*;
use construct::event::Event;
use construct::journal::Writer;
use construct::llm::Message;
use construct_dev::drive;
use serde_json::{Value, json};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> Running {
    let mut command = Command::new(construct());
    command.args(args).current_dir(&env.work).stdout(Stdio::null()).stderr(Stdio::null());
    for (k, v) in env.env() {
        if v.is_empty() {
            command.env_remove(k);
        } else {
            command.env(k, v);
        }
    }
    command.envs(extra.iter().copied());
    Running(command.spawn().unwrap())
}

fn until(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_millis(budget(8000));
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for durable boundary");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn play(d: &mut drive::Driver, script: &str) {
    let mut log = vec![];
    if let Err(e) = drive::run_script(d, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
}

fn open(env: &Env, id: &str) -> drive::Driver {
    drive::Driver::spawn(
        &[construct().display().to_string(), "--resume".into(), id.into()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        110,
        40,
    )
    .unwrap()
}

fn paired(req: &Value) {
    let mut pending = std::collections::BTreeSet::new();
    for m in req["messages"].as_array().unwrap() {
        if m["role"] == "tool" {
            assert!(pending.remove(m["tool_call_id"].as_str().unwrap()), "{req}");
        } else {
            assert!(pending.is_empty(), "no message may intervene before all tool results: {req}");
            for c in m["tool_calls"].as_array().into_iter().flatten() {
                assert!(pending.insert(c["id"].as_str().unwrap().to_string()));
            }
        }
    }
    assert!(pending.is_empty(), "{req}");
}

#[test]
fn kill_after_known_outcome_recovers_it_and_marks_unknown_effects_without_retry() {
    let env = Env::new(
        "recovery-outcomes",
        json!({"steps":[
            {"calls":[
                {"call":"shell","args":{"command":"echo known >> effects.txt; printf known-durable-output"}},
                {"call":"shell","args":{"command":"echo unknown >> effects.txt; sleep 300"}}
            ]},
            {"text":"Explicit continuation"}
        ]}),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut process = start(&env, &["run", "perform the two operations"], &[]);
    until(|| {
        env.journal().iter().filter(|r| r.ty == "tool_start").count() == 2
            && std::fs::read_to_string(env.work.join("effects.txt")).unwrap_or_default().contains("unknown")
    });
    assert_eq!(env.journal().iter().filter(|r| r.ty == "tool_end").count(), 1);
    let id = env.sessions()[0].clone();
    let refused = env.run(&["--resume", &id, "run", "must not execute"]);
    assert!(!refused.status.success());
    assert!(text(&refused).contains("in use by another process"), "{}", text(&refused));
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    let mut d = open(&env, &id);
    play(&mut d, "wait \"Previous turn was interrupted\" 5000\nkey ctrl-o\nwait \"known-durable-output\" 3000\n");
    let repaired = env.journal();
    assert_eq!(repaired.iter().filter(|r| r.ty == "recovered").count(), 1);
    let results: Vec<_> = repaired.iter().filter(|r| r.ty == "tool_result").collect();
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|r| r.payload["message"]["content"] == "known-durable-output"));
    assert!(results.iter().any(|r| r.payload.to_string().contains("External effects may already have happened")));
    assert_eq!(env.requests().len(), 1, "opening does not resume generation");
    play(&mut d, "type /quit\nkey enter\n");
    drop(d);
    let mut d = open(&env, &id);
    play(&mut d, "wait \"Previous turn was interrupted\" 5000\n");
    assert_eq!(env.journal().len(), repaired.len(), "second open is idempotent");
    play(&mut d, "type Continue explicitly\nkey enter\nwait \"Explicit continuation\" 5000\n");
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    paired(&reqs[1]);
    assert!(reqs[1].to_string().contains("known-durable-output"));
    assert_eq!(std::fs::read_to_string(env.work.join("effects.txt")).unwrap(), "known\nunknown\n");
    let _ = d;
}

#[test]
fn kill_during_streaming_preserves_prompt_but_does_not_claim_uncommitted_tokens() {
    let env = Env::new(
        "recovery-stream",
        json!({"steps":[
            {"text":"partially-streamed ".repeat(2000)}, {"text":"resumed explicitly"}
        ]}),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        100,
        32,
    )
    .unwrap();
    play(
        &mut d,
        "wait \"ask construct\" 5000\ntype Original streaming prompt\nkey enter\nwait \"partially-streamed\" 5000\n",
    );
    let id = env.sessions()[0].clone();
    d.kill();
    drop(d);
    until(|| {
        let file = env.journal_path(&env.work);
        Writer::resume(&file, &id).is_ok()
    });
    let mut d = open(&env, &id);
    play(
        &mut d,
        "wait \"Previous turn was interrupted\" 5000\nexpect \"Original streaming prompt\"\ngone \"partially-streamed\" 3000\ntype explicitly continue\nkey enter\nwait \"resumed explicitly\" 5000\n",
    );
    let reqs = env.requests();
    paired(reqs.last().unwrap());
    assert!(!reqs.last().unwrap().to_string().contains("partially-streamed"));
    assert!(reqs.last().unwrap().to_string().contains("Original streaming prompt"));
}

#[test]
fn kill_while_routing_recovers_original_prompt_before_model_facing_user_exists() {
    let env = Env::new("recovery-route", json!({"steps":[{"text":"route recovery answer"}]}));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/route", listener.local_addr().unwrap());
    env.config(&format!("[router]\nmode = 'jev'\njev_url = '{url}'\ntimeout_ms = 30000\n[learn]\nenabled = false\n"));
    let mut process = start(&env, &["run", "Raw prompt before routing"], &[("TYPESAFE_API_KEY", "test")]);
    let mut connection = None;
    until(|| match listener.accept() {
        Ok((stream, _)) => {
            connection = Some(stream);
            true
        }
        Err(_) => false,
    });
    until(|| env.journal().iter().any(|r| r.ty == "submitted"));
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    let rows = env.journal();
    assert!(!rows.iter().any(|r| r.ty == "user"));
    assert_eq!(rows.iter().find(|r| r.ty == "submitted").unwrap().payload["text"], "Raw prompt before routing");
    let id = env.sessions()[0].clone();
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let out = env.run(&["--resume", &id, "run", "continue"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].to_string().contains("Raw prompt before routing"));
    paired(&reqs[0]);
}

#[test]
fn recovered_jobs_wait_for_user_and_completed_results_are_delivered_once() {
    let env = Env::new("recovery-jobs", json!({"steps":[{"text":"delivered once"},{"text":"still once"}]}));
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    // Seed a process's committed boundary: one ended job awaiting delivery,
    // and one running job whose process no longer exists.
    let cfg = construct::config::Config::default();
    let (w, _) = Writer::create(&env.journal_path(&env.work), |id| Event::Head {
        id,
        harness: "construct".into(),
        version: "test".into(),
        cwd: env.work.display().to_string(),
        model: cfg.model.clone(),
        manifest: construct::manifest::Manifest::foreign(&cfg),
        parent_session: None,
        parent_call_id: None,
    })
    .unwrap();
    w.record(&Event::TurnStart);
    w.record(&Event::Submitted { text: "Run background work".into() });
    w.record(&Event::User { message: Message::user("Run background work") });
    for id in ["j1", "j2"] {
        w.record(&Event::JobStart(construct::jobs::Started {
            id: id.into(),
            kind: construct::jobs::Kind::Shell,
            label: "some external work".into(),
            call_id: "call".into(),
        }));
    }
    w.record(&Event::JobEnd(construct::jobs::Info {
        id: "j1".into(),
        kind: construct::jobs::Kind::Shell,
        label: "completed work".into(),
        status: construct::jobs::Status::Completed,
        exit: Some(0),
        ms: 10,
        tail: "completion pending delivery".into(),
    }));
    let id = w.session.clone();
    drop(w);
    let mut d = open(&env, &id);
    play(&mut d, "wait \"Previous turn was interrupted\" 5000\nsleep 300\n");
    assert!(env.requests().is_empty());
    assert!(env.journal().iter().any(|r| r.ty == "job_end" && r.payload["status"] == "interrupted"));
    play(
        &mut d,
        "type explicitly continue jobs\nkey enter\nwait \"delivered once\" 5000\ntype continue again\nkey enter\nwait \"still once\" 5000\n",
    );
    assert_eq!(env.journal().iter().filter(|r| r.ty == "jobs_delivered").count(), 1);
    assert_eq!(env.requests()[0].to_string().matches("completion pending delivery").count(), 1);
}

#[test]
fn unreadable_history_keeps_the_current_session_usable() {
    let env =
        Env::new("recovery-invalid", json!({"steps":[{"text":"healthy answer"},{"text":"healthy continuation"}]}));
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut d = drive::Driver::spawn(
        &[construct().display().to_string()],
        &env.env(),
        Some(env.work.to_str().unwrap()),
        110,
        38,
    )
    .unwrap();
    play(&mut d, "wait \"ask construct\" 5000\ntype Healthy prompt\nkey enter\nwait \"healthy answer\" 5000\n");
    let id = env.sessions()[0].clone();
    // A malformed saved session must not be silently truncated to a prefix.
    let db = rusqlite::Connection::open(env.journal_path(&env.work)).unwrap();
    db.execute("INSERT INTO events(session_id,type,v,at_ms,payload) SELECT 'broken',type,v,at_ms,payload FROM events WHERE session_id=?1", [&id]).unwrap();
    db.execute("INSERT INTO events(session_id,type,v,at_ms,payload) VALUES ('broken','future_event',1,0,'{\"event\":\"future_event\"}')", []).unwrap();
    play(
        &mut d,
        "type /resume broken\nkey enter\nwait \"unreadable or incompatible history\" 5000\nexpect \"healthy answer\"\ntype Continue healthy\nkey enter\nwait \"healthy continuation\" 5000\n",
    );
    assert_eq!(
        env.requests().last().unwrap()["messages"].as_array().unwrap().last().unwrap()["content"],
        "Continue healthy"
    );
    assert!(
        env.journal()
            .iter()
            .filter(|r| r.ty == "submitted")
            .all(|r| r.session == id || r.payload["text"] == "Healthy prompt")
    );
}

#[test]
fn journal_write_failure_is_visible_and_marks_recovery_degraded() {
    let env = Env::new("recovery-write-failure", json!({"steps":[{"text":"initial answer"},{"text":"second answer"}]}));
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    assert!(env.run(&["run", "initial prompt"]).status.success());
    let id = env.sessions()[0].clone();
    let db = rusqlite::Connection::open(env.journal_path(&env.work)).unwrap();
    db.execute_batch("CREATE TRIGGER fail_submitted BEFORE INSERT ON events WHEN new.type='submitted' BEGIN SELECT RAISE(FAIL, 'simulated journal write failure'); END;").unwrap();
    let out = env.run(&["--resume", &id, "run", "--json", "second prompt"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("journal write failed; recovery is degraded"), "{}", text(&out));
    assert!(text(&out).contains("simulated journal write failure"));
    assert_eq!(env.requests().len(), 2, "write failure is surfaced without claiming the turn was durable");
}
