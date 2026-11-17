//! What the client sends back and how long it waits (#47): a thinking
//! model's reasoning returns on the next request under the key it streamed
//! in, also after a resume, and a provider that stalls fails the turn instead
//! of hanging it.

use crate::common::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The assistant messages of a request body.
fn assistants(req: &Value) -> Vec<Value> {
    req["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "assistant").cloned().collect()
}

fn thinking_call() -> Value {
    json!({ "call": "read", "args": { "path": "notes.txt" }, "reasoning": "the notes say where it is" })
}

#[test]
fn reasoning_goes_back_with_its_tool_call_and_survives_a_resume() {
    let env = Env::new(
        "stream-reasoning",
        json!({ "steps": [thinking_call(), { "text": "found it" }, { "text": "again" }] }),
    );
    std::fs::write(env.work.join("notes.txt"), "here\n").unwrap();
    let out = env.run(&["run", "--no-learn", "where is it?"]);
    assert!(out.status.success(), "{}", text(&out));

    let reqs = env.requests();
    assert_eq!(reqs.len(), 2);
    let sent = assistants(&reqs[1]);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["reasoning_content"], "the notes say where it is");
    assert!(sent[0].get("reasoning").is_none(), "only the key the server streamed: {sent:?}");
    assert_eq!(sent[0]["tool_calls"][0]["function"]["name"].as_str().map(str::is_empty), Some(false));

    // The journal keeps the key, so a resumed session sends it too.
    let id = env.sessions()[0].clone();
    let out = env.run(&["--resume", &id, "run", "--no-learn", "and now?"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs = env.requests();
    let resumed = assistants(reqs.last().unwrap());
    assert_eq!(resumed[0]["reasoning_content"], "the notes say where it is", "{resumed:?}");
}

#[test]
fn reasoning_drop_keeps_it_off_the_wire() {
    let env = Env::new("stream-reasoning-drop", json!({ "steps": [thinking_call(), { "text": "found it" }] }));
    env.config("[provider]\nreasoning = \"drop\"\n");
    std::fs::write(env.work.join("notes.txt"), "here\n").unwrap();
    let out = env.run(&["run", "--no-learn", "where is it?"]);
    assert!(out.status.success(), "{}", text(&out));
    let sent = assistants(&env.requests()[1]);
    assert!(sent[0].get("reasoning_content").is_none(), "{sent:?}");
    // Still journaled for the transcript.
    let row = env.journal().into_iter().find(|r| r.ty == "assistant").unwrap();
    assert_eq!(row.payload["message"]["reasoning"], "the notes say where it is");
}

#[test]
fn a_stream_that_stalls_mid_answer_fails_instead_of_hanging() {
    let env = Env::new("stream-stall", json!({ "steps": [{ "text": "half of it", "stall_ms": 30000 }] }));
    env.config("[provider]\nidle_timeout = 1\n");
    let start = Instant::now();
    let out = env.run(&["run", "--no-learn", "hi"]);
    let took = start.elapsed();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("stalled") && text(&out).contains("idle_timeout"), "{}", text(&out));
    assert!(took < Duration::from_millis(budget(10_000)), "took {took:?}");
}

#[test]
fn a_silent_endpoint_fails_at_the_first_byte_deadline() {
    let env = Env::new("stream-silent", json!({ "steps": [{ "text": "late", "delay_ms": 30000 }] }));
    env.config("[provider]\nfirst_byte_timeout = 1\n");
    let start = Instant::now();
    let out = env.run(&["run", "--no-learn", "hi"]);
    let took = start.elapsed();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("first_byte_timeout"), "{}", text(&out));
    assert!(took < Duration::from_millis(budget(10_000)), "took {took:?}");
}

#[test]
fn stalled_http_error_bodies_fail_at_the_first_byte_deadline() {
    use axum::{body::Body, http::StatusCode};
    use futures::{StreamExt, stream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (status, prefix, tail) in [
        (StatusCode::BAD_REQUEST, "", r#"{"error":"late rejection"}"#),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"#,
            r#"{"code":"deck_credential_missing","message":"missing key"}}"#,
        ),
    ] {
        let env = Env::new(&format!("stream-error-body-{}", status.as_u16()), json!({ "steps": [] }));
        env.config("[provider]\nfirst_byte_timeout = 1\n");
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let port = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let app = axum::Router::new().route(
                "/v1/chat/completions",
                axum::routing::post(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let body = stream::once(async move { Ok::<_, std::io::Error>(prefix) }).chain(stream::once(
                            async move {
                                // Eventually finish, so the old bug fails without hanging the suite.
                                tokio::time::sleep(Duration::from_secs(3)).await;
                                Ok::<_, std::io::Error>(tail)
                            },
                        ));
                        (status, Body::from_stream(body))
                    }
                }),
            );
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            port
        });

        let out = env
            .run_with(&[("CONSTRUCT_BASE_URL", &format!("http://127.0.0.1:{port}/v1"))], &["run", "--no-learn", "hi"]);
        let output = text(&out);
        assert!(!out.status.success(), "{status}: {output}");
        assert!(output.contains("error body") && output.contains("first_byte_timeout"), "{status}: {output}");
        assert!(output.contains(&status.as_u16().to_string()), "{output}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a stalled error body must not be retried");
        assert_eq!(env.journal().iter().filter(|r| r.ty == "error").count(), 1);
    }
}

#[tokio::test]
async fn an_unreadable_http_error_body_keeps_its_status_and_cause() {
    use construct::config::Config;
    use construct::endpoint::Endpoint;
    use construct::llm::{Client, Message, Request};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).await.unwrap() > 0, "client closed before sending its request");
        socket
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 1000\r\nConnection: close\r\n\r\npartial")
            .await
            .unwrap();
    });
    let cfg = Config { base_url: url, ..Config::default() };
    let client = Client::new(&Endpoint::declared(&cfg), None).unwrap();
    let messages = [Message::user("hi")];
    let request = Request { model: "mock", messages: &messages, tools: &[], max_tokens: None, temperature: None };
    let error = tokio::time::timeout(Duration::from_secs(5), client.stream(request, None)).await.unwrap().unwrap_err();
    let message = error.to_string();
    assert!(message.contains("400") && message.contains("unreadable error body"), "{error:#}");
    assert!(error.chain().count() > 1, "body-read failure lost its cause: {error:#}");
    server.await.unwrap();
}

/// A local HTTP response whose final data line has no trailing newline.
fn unterminated(env: &Env, body: &'static str) -> std::process::Output {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let port = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || async move { ([("content-type", "text/event-stream")], body) }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    });
    env.run_with(&[("CONSTRUCT_BASE_URL", &format!("http://127.0.0.1:{port}/v1"))], &["run", "--no-learn", "hi"])
}

#[test]
fn eof_keeps_the_final_text_finish_reason_and_usage() {
    let env = Env::new("stream-eof-json", json!({ "steps": [] }));
    let out = unterminated(
        &env,
        r#"data: {"choices":[{"delta":{"content":"last 🌱"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("last 🌱"));
    let rows = env.journal();
    let assistant = rows.iter().find(|r| r.ty == "assistant").unwrap();
    assert_eq!(assistant.payload["message"]["content"], "last 🌱");
    let usage = rows.iter().find(|r| r.ty == "usage").unwrap();
    assert_eq!((usage.payload["prompt"].as_u64(), usage.payload["completion"].as_u64()), (Some(7), Some(3)));
    assert!(!rows.iter().any(|r| r.ty == "note"), "{rows:?}");
}

#[test]
fn eof_recognizes_done_without_a_spurious_truncation_note() {
    let env = Env::new("stream-eof-done", json!({ "steps": [] }));
    let out = unterminated(&env, "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]");
    assert!(out.status.success(), "{}", text(&out));
    assert!(!env.journal().iter().any(|r| r.ty == "note"));
}

#[test]
fn eof_surfaces_a_final_provider_error() {
    let env = Env::new("stream-eof-error", json!({ "steps": [] }));
    let out = unterminated(&env, r#"data: {"error":{"message":"final provider failure"}}"#);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("final provider failure"), "{}", text(&out));
    assert!(env.journal().iter().any(|r| r.ty == "error"));
}
