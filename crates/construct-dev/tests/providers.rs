//! Named providers from the user's side (ADR 0027): two endpoints, each with
//! its own key and the same model id, switched with `/model`; a bare id two
//! providers list is refused; a loopback provider is sent no key.

use crate::common::*;
use construct_dev::drive;
use serde_json::{Value, json};

/// `a` and `b` both serve `x`, each on its own scripted endpoint with its own key.
fn two(a: &Env, b: &Env) -> String {
    format!(
        "model = \"a/x\"\n[router]\nmode = \"local\"\n[learn]\nenabled = false\n\
         [providers.a]\nbase_url = \"http://127.0.0.1:{}/v1\"\napi_key_secret = \"A_KEY\"\n\
         [providers.a.models.x]\ncontext_window = 64000\n\
         [providers.b]\nbase_url = \"http://127.0.0.1:{}/v1\"\napi_key_secret = \"B_KEY\"\n\
         [providers.b.models.x]\ncontext_window = 32000\n",
        a.port, b.port
    )
}

const KEYS: [(&str, &str); 2] = [("A_KEY", "aaaa-1"), ("B_KEY", "bbbb-2")];

fn run(d: &mut drive::Driver, log: &mut Vec<u8>, script: &str) {
    if let Err(e) = drive::run_script(d, script, log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(log));
    }
}

#[test]
fn switching_providers_sends_each_request_to_its_own_endpoint_with_its_own_key() {
    let a = Env::new("providers-a", json!({ "steps": [{ "text": "from-a" }] }));
    let b = Env::new("providers-b", json!({ "steps": [{ "text": "from-b" }] }));
    a.config(&two(&a, &b));
    let mut env = a.env();
    env.extend(KEYS.map(|(k, v)| (k.to_string(), v.to_string())));
    let mut d =
        drive::Driver::spawn(&[construct().display().to_string()], &env, Some(a.work.to_str().unwrap()), 120, 34)
            .unwrap();
    let mut log = Vec::new();
    let (short, long) = (budget(3000), budget(5000));
    run(&mut d, &mut log, &format!("wait \"ask construct\" {short}\ntype hello\nkey enter\nwait \"from-a\" {long}\n"));
    run(&mut d, &mut log, &format!("type /model b/x\nkey enter\nwait \"model → b/x · 32k ctx\" {short}\n"));
    run(&mut d, &mut log, &format!("type again\nkey enter\nwait \"from-b\" {long}\n"));
    d.kill();

    // Each endpoint saw only its own request, its own key and the plain id.
    assert_eq!(a.auth_seen(), vec![json!("Bearer aaaa…")]);
    assert_eq!(b.auth_seen(), vec![json!("Bearer bbbb…")]);
    assert_eq!(a.requests()[0]["model"], "x");
    assert_eq!(b.requests()[0]["model"], "x");

    let rows = a.journal();
    let head = rows.iter().find(|r| r.ty == "head").unwrap();
    assert_eq!(head.payload["model"], "a/x");
    assert_eq!(head.payload["manifest"]["provider"], "a");
    assert_eq!(head.payload["manifest"]["context_window"], 64_000, "the provider's entry sets the window");
    let sw: Vec<&Row> = rows.iter().filter(|r| r.ty == "model").collect();
    assert_eq!(sw.len(), 1);
    assert_eq!((&sw[0].payload["model"], &sw[0].payload["provider"]), (&json!("b/x"), &json!("b")));
}

#[test]
fn a_bare_id_two_providers_list_is_refused_before_any_request() {
    let a = Env::new("providers-ambiguous-a", json!({ "steps": [{ "text": "no" }] }));
    let b = Env::new("providers-ambiguous-b", json!({ "steps": [{ "text": "no" }] }));
    a.config(&two(&a, &b));
    let out = a.run_with(&KEYS, &["--model", "x", "run", "--no-learn", "go"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("\"x\" is served by more than one provider; pick one: a/x, b/x"), "{err}");
    assert!(a.auth_seen().is_empty() && b.auth_seen().is_empty(), "a request was sent");

    // In config.toml the same mistake names the setting.
    a.config(&two(&a, &b).replace("model = \"a/x\"", "model = \"x\""));
    let out = a.run_with(&KEYS, &["run", "--no-learn", "go"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("model: \"x\" is served by more than one provider"));
}

#[test]
fn a_loopback_provider_gets_no_key_and_a_missing_key_names_the_provider() {
    let a = Env::new("providers-keyless", json!({ "steps": [{ "text": "local-ok" }] }));
    let url = format!("http://127.0.0.1:{}/v1", a.port);
    a.config(&format!("model = \"local/m\"\n[providers.local]\nbase_url = \"{url}\"\n"));
    let out = a.run_with(&[("OPENROUTER_API_KEY", "orou-LEAK")], &["run", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(a.auth_seen(), vec![Value::Null], "a loopback provider takes no key, and never the fallback");
    assert_eq!(a.requests()[0]["model"], "m");

    let b = Env::new("providers-missing-key", json!({ "steps": [{ "text": "no" }] }));
    b.config(&format!("model = \"gpubox/m\"\n[providers.gpubox]\nbase_url = \"http://127.0.0.1:{}/v1\"\napi_key_secret = \"GPUBOX_API_KEY\"\n", b.port));
    let out = b.run_with(&[("GPUBOX_API_KEY", ""), ("OPENROUTER_API_KEY", "orou-LEAK")], &["run", "--no-learn", "go"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("gpubox: no API key: set GPUBOX_API_KEY"), "{err}");
    assert!(b.auth_seen().is_empty());
}

#[test]
fn a_child_on_another_provider_reaches_only_that_endpoint_with_only_that_key() {
    let a = Env::new(
        "providers-child-a",
        json!({ "steps": [{ "call": "delegate", "args": { "agent": "scout", "task": "hi" } }, { "text": "done" }] }),
    );
    let b = Env::new("providers-child-b", json!({ "steps": [{ "text": "child-says" }] }));
    let agent =
        "[agents.scout]\ndescription = \"reads\"\nprompt = \"You are a scout.\"\ntools = [\"read\"]\nmodel = \"b/x\"\n";
    a.config(&format!("{}{agent}", two(&a, &b)));
    let out = a.run_with(&KEYS, &["run", "--no-learn", "go"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(a.auth_seen(), vec![json!("Bearer aaaa…"); 2], "parent, then parent again");
    assert_eq!(b.auth_seen(), vec![json!("Bearer bbbb…")], "the child: b's key, never a's");
    assert_eq!(b.requests()[0]["model"], "x");
}
