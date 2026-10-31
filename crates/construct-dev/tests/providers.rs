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

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn providers_lists_each_one_and_test_probes_only_the_one_named() {
    let a = Env::new("providers-cli-a", json!({ "side": ["ok"], "steps": [{ "call": "ping", "args": {} }] }));
    let b = Env::new("providers-cli-b", json!({ "steps": [{ "text": "no" }] }));
    a.config(&two(&a, &b));
    let list = stdout(&a.run_with(&KEYS, &["providers"]));
    let row = |name: &str, port: u16, key: &str| {
        list.lines().any(|l| {
            l.split_whitespace().collect::<Vec<_>>().starts_with(&[
                name,
                &format!("http://127.0.0.1:{port}/v1"),
                "openai",
                key,
            ])
        })
    };
    assert!(row("a", a.port, "A_KEY") && row("b", b.port, "B_KEY"), "{list}");
    assert!(list.lines().any(|l| l.starts_with("default ")), "{list}");
    assert!(a.auth_seen().is_empty(), "list sends nothing");

    let out = a.run_with(&KEYS, &["providers", "test", "a", "--model", "mock"]);
    let text = stdout(&out);
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    for want in [
        "models  ok      2 listed",
        "reply   ok      \"ok\" from mock",
        "tools   ok      called ping",
        "window  131072 (stated by the server)",
    ] {
        assert!(text.contains(want), "missing {want:?}\n{text}");
    }
    assert_eq!(a.auth_seen(), vec![json!("Bearer aaaa…"); 2], "the reply and the tool call, with a's key");
    assert_eq!(a.requests()[1]["tools"][0]["function"]["name"], "ping");
    assert!(b.auth_seen().is_empty(), "b was not probed");
}

#[test]
fn providers_test_fails_on_a_model_that_wont_call_tools() {
    let a = Env::new("providers-cli-notools", json!({ "side": ["ok"], "steps": [{ "text": "I would rather not" }] }));
    let b = Env::new("providers-cli-notools-b", json!({ "steps": [] }));
    a.config(&two(&a, &b));
    let out = a.run_with(&KEYS, &["providers", "test", "a"]);
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("reply   ok      \"ok\" from x"),
        "the catalogued id is the default\n{}",
        stdout(&out)
    );
    assert!(stdout(&out).contains("tools   FAILED  no tool call"), "{}", stdout(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("a: 1 of 3 checks failed"));
}

#[test]
fn an_error_from_a_named_provider_says_which_one_its_url_and_key() {
    let a = Env::new("providers-error", json!({ "steps": [{ "text": "never" }] }));
    // The fragment is not sent, so the request lands on /v1 and the mock answers 404.
    let url = format!("http://127.0.0.1:{}/v1#", a.port);
    a.config(&format!("model = \"c/m\"\n[providers.c]\nbase_url = \"{url}\"\n"));
    let out = a.run(&["run", "--no-learn", "go"]);
    let all = format!("{}{}", stdout(&out), String::from_utf8_lossy(&out.stderr));
    assert!(all.contains(&format!("c ({url}, no key): model endpoint returned 404")), "{all}");
}

#[test]
fn a_named_providers_model_list_sets_the_window_at_startup_and_is_cached() {
    let a = Env::new("providers-discover", json!({ "steps": [{ "text": "ok" }] }));
    // The scripted endpoint lists `mock/small` with a 32768-token window.
    a.config(&format!("model = \"a/mock/small\"\n[providers.a]\nbase_url = \"http://127.0.0.1:{}/v1\"\n", a.port));
    let out = a.run(&["run", "--no-learn", "--json", "go"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let head = a.journal().into_iter().find(|r| r.ty == "head").unwrap();
    assert_eq!(head.payload["manifest"]["context_window"], 32_768, "the window the server's list states");
    assert_eq!(a.requests()[0]["model"], "mock/small");
    let list = stdout(&a.run(&["providers"]));
    assert!(list.lines().any(|l| l.starts_with("a ") && l.contains("2 listed")), "the list is cached\n{list}");

    // `off` asks nothing: the default window stays.
    let b = Env::new("providers-discover-off", json!({ "steps": [{ "text": "ok" }] }));
    let cfg = format!(
        "model = \"a/mock/small\"\n[providers.a]\nbase_url = \"http://127.0.0.1:{}/v1\"\ndiscover = \"off\"\n",
        b.port
    );
    b.config(&cfg);
    assert!(b.run(&["run", "--no-learn", "go"]).status.success());
    let head = b.journal().into_iter().find(|r| r.ty == "head").unwrap();
    assert_eq!(head.payload["manifest"]["context_window"], 131_072);
}
