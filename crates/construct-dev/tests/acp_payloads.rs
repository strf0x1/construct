//! Payload conformance against the pinned shared contract. Fixtures prove the
//! schemas discriminate; real ACP exchanges prove Construct uses those shapes.
//! All references are registered locally, with network resolution disabled.

use crate::acp::Client;
use crate::common::*;
use construct::acp::map;
use construct::tools::boundary::Notice;
use jsonschema::{Registry, Validator};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

fn vendored() -> PathBuf {
    let lock: toml::Value = toml::from_str(&std::fs::read_to_string(root().join("CONTRACT.lock")).unwrap()).unwrap();
    root().join(lock["vendored"].as_str().unwrap())
}

fn load(path: impl AsRef<std::path::Path>) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

pub(crate) fn validators() -> HashMap<String, Validator> {
    let schemas: Vec<_> = std::fs::read_dir(vendored().join("schema"))
        .unwrap()
        .map(|p| {
            let path = p.unwrap().path();
            (path.file_name().unwrap().to_str().unwrap().to_string(), load(path))
        })
        .collect();
    let mut registry = Registry::new();
    for (_, schema) in &schemas {
        registry = registry.add(schema["$id"].as_str().unwrap(), schema.clone()).unwrap();
    }
    let registry = registry.prepare().unwrap();
    schemas
        .iter()
        .map(|(name, schema)| (name.clone(), jsonschema::options().with_registry(&registry).build(schema).unwrap()))
        .collect()
}

pub(crate) fn check(schemas: &HashMap<String, Validator>, name: &str, value: &Value) {
    let errors: Vec<_> = schemas[&format!("{name}.schema.json")].iter_errors(value).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{name}: {errors:?}\npayload: {value}");
}

#[test]
fn shared_positive_and_negative_payload_fixtures() {
    let schemas = validators();
    let fixtures = load(vendored().join("payloads.json"));
    let version = std::fs::read_to_string(vendored().join("VERSION")).unwrap();
    assert_eq!(fixtures["contract"], version.trim());
    let mut covered = std::collections::HashSet::new();
    for case in fixtures["cases"].as_array().unwrap() {
        let name = case["schema"].as_str().unwrap();
        covered.insert(name);
        assert_eq!(schemas[name].is_valid(&case["payload"]), case["valid"].as_bool().unwrap(), "{}", case["case"]);
        // The actual receiver must accept the valid canonical and alias forms,
        // and reject malformed identifiers instead of queueing them.
        if name == "boundary-notice.schema.json" {
            assert_eq!(
                serde_json::from_value::<Notice>(case["payload"].clone()).is_ok_and(|notice| notice.validate().is_ok()),
                case["valid"].as_bool().unwrap(),
                "boundary decoder: {}",
                case["case"]
            );
        }
    }
    assert_eq!(covered.len(), schemas.len(), "every schema must have a payload fixture");
}

#[test]
fn actual_agent_capabilities_events_turns_and_load_snapshots_conform() {
    let schemas = validators();
    // Both negotiated names exercise the production emitter, not a hand-built
    // copy of an example. EOF/restart also exercises the disk load response.
    for key in [map::META_KEY, map::LEGACY_META_KEY] {
        let outside = Env::dir_for("acp-payloads-outside").join("note.txt");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, "permission fixture").unwrap();
        let env = Env::new(
            "acp-payloads",
            json!({ "steps": [
            { "call": "read", "args": { "path": outside } }, { "text": "contract payload" }
        ] }),
        );
        let boundary = boundary::Server::start();
        let mut c = Client::start_with(&env, &[("DECK_BOUNDARY_URL", boundary.url().as_str())]);
        c.answers.push_back("once");
        let (init, _) = c.call(
            "initialize",
            json!({
                "protocolVersion": 1, "clientCapabilities": { "_meta": { key: { "events": true, "turn": true } } }
            }),
        );
        check(&schemas, "agent-capabilities", &init["result"]["agentCapabilities"]["_meta"]);
        for namespace in [map::META_KEY, map::LEGACY_META_KEY] {
            assert_eq!(init["result"]["agentCapabilities"]["_meta"][namespace]["liveAttach"], true);
        }
        let (new, mut notes) = c.call("session/new", json!({ "cwd": env.work, "mcpServers": [] }));
        let sid = new["result"]["sessionId"].as_str().unwrap();
        let (done, turn_notes) = c.call(
            "session/prompt",
            json!({
                "sessionId": sid, "prompt": [{ "type": "text", "text": "go" }]
            }),
        );
        assert_eq!(done["result"]["stopReason"], "end_turn", "{done}");
        notes.extend(turn_notes);
        let mut events = 0;
        let mut turns = 0;
        let mut waiting = false;
        for note in notes {
            match note["method"].as_str() {
                Some(map::EVENT_METHOD | map::LEGACY_EVENT_METHOD) => {
                    check(&schemas, "event-notification", &note["params"]);
                    events += 1;
                }
                Some(map::TURN_METHOD | map::LEGACY_TURN_METHOD) => {
                    check(&schemas, "turn-notice", &note["params"]);
                    waiting |= note["params"]["state"] == "waiting";
                    turns += 1;
                }
                _ => {}
            }
        }
        assert!(events > 0 && turns > 0, "must observe actual events and turn transitions");
        assert!(waiting, "must observe a real permission wait transition");
        c.finish();
        let mut c = Client::start(&env);
        c.call("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
        let (loaded, _) = c.call("session/load", json!({ "sessionId": sid, "cwd": env.work, "mcpServers": [] }));
        check(&schemas, "load-snapshot", &loaded["result"]);
        assert_eq!(loaded["result"]["_meta"][map::META_KEY], loaded["result"]["_meta"][map::LEGACY_META_KEY]);
        c.finish();
    }
}

#[test]
fn actual_parent_capabilities_and_child_start_conform() {
    use construct::acp::{Start, client};
    use construct::agents::Gate;
    use construct::child::{Origin, run::Outcome, wire::Wire};
    use construct::config::Config;
    use construct::sandbox::Policy;
    use construct::tools::Call;

    let schemas = validators();
    let env = Env::new("acp-parent-payloads", json!({ "steps": [] }));
    let cfg = Config::default();
    let paths = construct::config::Paths { home: env.home.clone() };
    let start = Start {
        config: Box::new(cfg.clone()),
        origin: Box::new(Origin {
            policy: Policy::new(&cfg.sandbox, &env.work, &paths),
            system: "child".into(),
            parent_session: "parent".into(),
            parent_call_id: "call".into(),
        }),
    };
    let mut wire = client::Client::construct(&env.work, "task", start, Gate::reject());
    let init = load_line(&wire.open()[0]);
    check(&schemas, "client-capabilities", &init["params"]["clientCapabilities"]["_meta"]);
    let reply = json!({ "jsonrpc": "2.0", "id": init["id"], "result": {
        "agentInfo": { "version": "test" }, "agentCapabilities": { "_meta": { map::META_KEY: { "events": true } } }
    } });
    let next = wire.line(&reply.to_string(), &Call::detached("call"), &mut Outcome::default());
    let new = load_line(&next[0]);
    check(&schemas, "start", &new["params"]["_meta"][map::META_KEY]["start"]);
}

fn load_line(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}
