//! The child end of delegation: `construct __child`, driven over its stdin and
//! stdout exactly as a parent drives it. No parent here, so these pin the wire
//! and what a child is (its prompt, tools, head row and home).

use crate::common::*;
use construct::agents::AgentDef;
use construct::child::wire::{Origin, ToChild};
use construct::config::Config;
use construct::event::Event;
use construct::sandbox::Policy;
use construct::tools::Catalog;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

const SYSTEM: &str = "You are a scout. Report what you find.";

/// A child running against `env`'s scripted model, set up as `def` under the
/// default parent config, with its own scratch home.
fn spawn(env: &Env, def: &AgentDef) -> (Child, std::path::PathBuf) {
    let cfg = Config { base_url: format!("http://127.0.0.1:{}/v1", env.port), ..Default::default() };
    let table = Catalog::build(&cfg.tools, None).unwrap().table();
    let cfg = def.child_config("scout", &cfg, &table).unwrap();
    let scratch = env.dir.join("child-home");
    let mut c = Command::new(construct());
    c.arg("__child").current_dir(&env.work).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env.env() {
        if v.is_empty() {
            c.env_remove(k);
        } else {
            c.env(k, v);
        }
    }
    c.env("CONSTRUCT_HOME", &scratch);
    let mut child = c.spawn().unwrap();
    let policy = Policy { off: true, ..Default::default() };
    let origin =
        Origin { policy, system: SYSTEM.into(), parent_session: "parent-1".into(), parent_call_id: "call-7".into() };
    let start = ToChild::Start { config: Box::new(cfg), cwd: env.work.clone(), origin: Box::new(origin) };
    child.stdin.as_mut().unwrap().write_all(start.line().as_bytes()).unwrap();
    (child, scratch)
}

fn send(child: &mut Child, msg: &ToChild) {
    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(msg.line().as_bytes()).unwrap();
    stdin.flush().unwrap();
}

/// The child's stdout, read as it arrives.
struct Wire {
    rx: std::sync::mpsc::Receiver<Event>,
    seen: Vec<Event>,
}

impl Wire {
    fn new(child: &mut Child) -> Self {
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(ev) = construct::child::wire::parse_event(&l) {
                    let _ = tx.send(ev);
                }
            }
        });
        Self { rx, seen: vec![] }
    }

    /// Read until the model's slow response has opened, so a cancel sent
    /// next lands inside the turn (before it, the child rightly ignores it).
    fn until_opened(&mut self) {
        let limit = std::time::Duration::from_millis(budget(10_000));
        loop {
            let ev = self
                .rx
                .recv_timeout(limit)
                .unwrap_or_else(|_| panic!("no response stream in {limit:?}: {:?}", self.seen));
            self.seen.push(ev.clone());
            if matches!(ev, Event::Opened) {
                return;
            }
        }
    }

    /// Read until the next turn ends; returns whether it was interrupted.
    fn turn_end(&mut self) -> bool {
        let limit = std::time::Duration::from_millis(budget(10_000));
        loop {
            let ev =
                self.rx.recv_timeout(limit).unwrap_or_else(|_| panic!("no turn end in {limit:?}: {:?}", self.seen));
            self.seen.push(ev.clone());
            if let Event::TurnEnd { interrupted, .. } = ev {
                return interrupted;
            }
        }
    }

    /// Everything until the child closes stdout.
    fn finish(mut self) -> Vec<Event> {
        self.seen.extend(self.rx.iter());
        self.seen
    }
}

fn slow_read() -> serde_json::Value {
    json!({ "call": "read", "args": { "path": "note.txt" }, "delay_ms": 20000 })
}

#[test]
fn a_child_runs_its_prompt_with_only_its_tools_and_names_its_parent() {
    let env = Env::new(
        "child-turn",
        json!({ "steps": [
            { "call": "read", "args": { "path": "note.txt" } },
            { "text": "found: hello" }
        ] }),
    );
    std::fs::write(env.work.join("note.txt"), "hello\n").unwrap();
    let def = AgentDef { tools: vec!["read".into()], ..Default::default() };
    let (mut child, scratch) = spawn(&env, &def);
    let mut wire = Wire::new(&mut child);
    send(&mut child, &ToChild::Prompt { text: "look at note.txt".into() });
    assert!(!wire.turn_end());
    drop(child.stdin.take()); // the parent is done with it: the child leaves
    let evs = wire.finish();
    assert!(child.wait().unwrap().success());

    // Opens as a child session naming its parent and the call that made it.
    let Some(Event::Head { parent_session, parent_call_id, .. }) = evs.first() else { panic!("{evs:?}") };
    assert_eq!((parent_session.as_deref(), parent_call_id.as_deref()), (Some("parent-1"), Some("call-7")));
    let answer = evs.iter().rev().find_map(|e| match e {
        Event::Assistant { message } => message.content.clone(),
        _ => None,
    });
    assert_eq!(answer.as_deref(), Some("found: hello"));

    // The model saw the child's prompt and only the child's tools.
    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(tool_names(&reqs[0]), vec!["read"]);
    assert_eq!(reqs[0]["messages"][0]["content"], SYSTEM);

    // Its journal is scratch in its own home; the parent's home holds none of it.
    assert!(scratch.join("projects").exists());
    assert!(env.journal().is_empty());
}

#[test]
fn closing_stdin_stops_a_turn_in_flight() {
    let env = Env::new("child-eof", json!({ "steps": [ slow_read() ] }));
    let def = AgentDef { tools: vec!["read".into()], ..Default::default() };
    let (mut child, _) = spawn(&env, &def);
    let mut wire = Wire::new(&mut child);
    send(&mut child, &ToChild::Prompt { text: "go".into() });
    std::thread::sleep(std::time::Duration::from_millis(500));
    drop(child.stdin.take());
    let started = std::time::Instant::now();
    assert!(wire.turn_end(), "the turn was interrupted");
    child.wait().unwrap();
    assert!(started.elapsed().as_millis() < budget(5000) as u128, "the child outlived its input");
}

#[test]
fn a_cancel_line_interrupts_but_the_child_stays_up_for_the_next_prompt() {
    let env = Env::new("child-cancel", json!({ "steps": [ slow_read(), { "text": "second answer" } ] }));
    let def = AgentDef { tools: vec!["read".into()], ..Default::default() };
    let (mut child, _) = spawn(&env, &def);
    let mut wire = Wire::new(&mut child);
    send(&mut child, &ToChild::Prompt { text: "first".into() });
    wire.until_opened();
    send(&mut child, &ToChild::Cancel);
    assert!(wire.turn_end(), "cancel interrupted the first turn");
    send(&mut child, &ToChild::Prompt { text: "second".into() });
    assert!(!wire.turn_end(), "the child answered the next prompt");
    drop(child.stdin.take());
    let evs = wire.finish();
    child.wait().unwrap();
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::Assistant { message } if message.content.as_deref() == Some("second answer")))
    );
}
