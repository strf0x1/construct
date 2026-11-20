//! A real Construct request through the recording proxy: affinity and USD usage survive,
//! while the proxy's upstream credential replaces the harness placeholder.

use crate::common::*;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::time::Duration;

struct ProxyProcess(std::process::Child);

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn proxy_cli_resolves_only_the_declared_secret_and_never_the_openrouter_fallback() {
    let upstream = Env::new("proxy-key-secret", json!({ "steps": [], "side": ["key worked"] }));
    let port_file = upstream.dir.join("proxy-port");
    let stderr = upstream.dir.join("proxy-stderr");
    let mut proxy = ProxyProcess(
        std::process::Command::new(dev_bin())
            .args(["serve", "--upstream", &format!("http://127.0.0.1:{}/v1", upstream.port)])
            .args(["--key-secret", "CONSTRUCT_TEST_PROXY_KEY", "--port-file"])
            .arg(&port_file)
            .env("CONSTRUCT_TEST_PROXY_KEY", "proxy-own-key")
            .env("OPENROUTER_API_KEY", "wrong-fallback-key")
            .env("SECRETSPEC_REASON", "Test declared benchmark proxy credential resolution")
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_millis(budget(5000));
    let port = loop {
        if let Ok(port) = std::fs::read_to_string(&port_file) {
            break port;
        }
        assert!(
            proxy.0.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline,
            "{}",
            std::fs::read_to_string(&stderr).unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let reply: Value = rt.block_on(async {
        construct_core::http::builder()
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .bearer_auth("harness-placeholder")
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    assert_eq!(reply["choices"][0]["message"]["content"], "key worked");
    assert_eq!(upstream.auth_seen(), vec![json!("Bearer prox…")]);
}

#[test]
fn proxy_keeps_orca_affinity_and_cost_without_forwarding_unrelated_headers() {
    let env = Env::new("proxy-orca", json!({ "steps": [] }));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let proxy = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer proxy-upstream-key");
                assert_eq!(headers["x-orcarouter-session-id"], "conversation-1");
                assert_eq!(headers["x-orcarouter-include-cost"], "true");
                assert!(headers.get("cookie").is_none());
                assert!(headers.get("x-unrelated").is_none());
                assert_eq!(body["stream_options"]["include_usage"], true);
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("x-orca-request-id", "receipt-1"),
                        ("x-orca-resolved-model", "deepseek/fixed"),
                    ],
                    concat!(
                        "data: {\"model\":\"deepseek/fixed\",\"choices\":[{\"delta\":{\"content\":\"cached reply\"},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":90},\"cost_usd\":0.002}}\n\n",
                        "data: [DONE]\n\n"
                    ),
                )
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cfg = construct_dev::serve::Config {
            script: vec![],
            repeat_call: None,
            side: vec![],
            upstream: Some(upstream),
            key: Some("proxy-upstream-key".into()),
            log: Some(env.dir.join("proxy.jsonl")),
            dump_dir: None,
            chunk_ms: 0,
            window: None,
            bytes_per_token: 4.0,
            cache: false,
            models_ms: 0,
        };
        construct_dev::serve::serve(cfg, 0).await.unwrap().0
    });
    env.config(
        "[learn]\nenabled = false\n[router]\nmode = \"local\"\n\
         [headers]\nX-OrcaRouter-Session-Id = \"conversation-1\"\nX-OrcaRouter-Include-Cost = \"true\"\n\
         Cookie = \"must-not-forward\"\nX-Unrelated = \"must-not-forward\"\n",
    );
    let out =
        env.run_with(&[("CONSTRUCT_BASE_URL", &format!("http://127.0.0.1:{proxy}/v1"))], &["run", "--json", "hi"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = env.journal();
    let usage = rows.iter().find(|r| r.ty == "usage").unwrap();
    assert_eq!(usage.payload["cached"], 90);
    assert_eq!(usage.payload["cost"], 0.002);
    let turn = rows.iter().find(|r| r.ty == "turn_end").unwrap();
    assert_eq!(turn.payload["cost"], 0.002);
    // The logger writes at stream completion, just after the downstream receives it.
    let path = env.dir.join("proxy.jsonl");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let record = loop {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(record) = serde_json::from_str::<Value>(text.trim())
        {
            break record;
        }
        assert!(std::time::Instant::now() < deadline, "proxy did not finish its log record");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(record["upstream_request_id"], "receipt-1");
    assert_eq!(record["resolved_model"], "deepseek/fixed");
    assert_eq!(record["usage"]["cost_usd"], 0.002);
    assert!(!record.to_string().contains("proxy-upstream-key"));
    let receipt_id = rt.block_on(async {
        let response = construct_core::http::builder()
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{proxy}/v1/chat/completions"))
            .header("x-orcarouter-session-id", "conversation-1")
            .header("x-orcarouter-include-cost", "true")
            .json(&json!({ "model": "deepseek/fixed", "stream": true, "messages": [] }))
            .send()
            .await
            .unwrap();
        let receipt_id = response.headers()["x-orca-request-id"].clone();
        assert!(response.text().await.unwrap().contains("[DONE]"));
        receipt_id
    });
    assert_eq!(receipt_id, "receipt-1");
}
