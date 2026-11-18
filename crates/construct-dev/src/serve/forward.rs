//! Forward completions to a configured upstream and record their streamed usage.

use super::{Shared, auth, event_stream, headers, ms, over_window, tokio_stream, write_log};
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) async fn request(
    s: Arc<Shared>,
    up: String,
    headers: HeaderMap,
    uri: axum::http::Uri,
    req: &mut Value,
    mut rec: Value,
) -> Response {
    let stream = req["stream"].as_bool().unwrap_or(false);
    if stream {
        req["stream_options"] = json!({ "include_usage": true });
    }
    let path = uri.path().trim_start_matches("/api").trim_start_matches("/v1");
    let mut rb = s.http.post(format!("{up}{path}")).json(&*req);
    if let Some(k) = auth(&s, &headers) {
        rb = rb.bearer_auth(k);
    }
    let rb = headers::forward(&headers, rb, &up);
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            rec["error"] = json!(e.to_string());
            write_log(&s, &rec);
            return (StatusCode::BAD_GATEWAY, e.to_string()).into_response();
        }
    };
    let request_id = resp.headers().get(headers::ORCA_REQUEST_ID).cloned();
    headers::record(resp.headers(), &mut rec);
    let mut response = forward_response(s, resp, stream, rec).await;
    if let Some(request_id) = request_id {
        response.headers_mut().insert(headers::ORCA_REQUEST_ID, request_id);
    }
    response
}

async fn forward_response(s: Arc<Shared>, resp: reqwest::Response, stream: bool, mut rec: Value) -> Response {
    let status = resp.status().as_u16();
    rec["status"] = json!(status);
    if !stream || status != 200 {
        let bytes = resp.bytes().await.unwrap_or_default();
        rec["t_first_byte"] = json!(ms(&s));
        rec["t_end"] = rec["t_first_byte"].clone();
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            rec["usage"] = v["usage"].clone();
            rec["resp_tool_calls"] = json!(
                v.pointer("/choices/0/message/tool_calls").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)
            );
            rec["finish_reason"] = v.pointer("/choices/0/finish_reason").cloned().unwrap_or(Value::Null);
            if status != 200 {
                rec["error"] = json!(String::from_utf8_lossy(&bytes).chars().take(400).collect::<String>());
            }
        }
        write_log(&s, &rec);
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        return (status, [(CONTENT_TYPE, "application/json")], Body::from(bytes)).into_response();
    }
    if s.cfg.window.is_some() {
        return windowed(s, resp, rec).await;
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let s2 = s.clone();
    tokio::spawn(async move {
        let mut body = resp.bytes_stream();
        let mut first = true;
        let mut buf = String::new();
        let mut calls = 0usize;
        let mut usage = Value::Null;
        let mut finish = Value::Null;
        while let Some(chunk) = body.next().await {
            let Ok(chunk) = chunk else { break };
            if first {
                rec["t_first_byte"] = json!(ms(&s2));
                first = false;
            }
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(p) = buf.find('\n') {
                let line: String = buf.drain(..=p).collect();
                if let Some(d) = line.trim().strip_prefix("data:")
                    && let Ok(v) = serde_json::from_str::<Value>(d.trim())
                {
                    if v["usage"].is_object() {
                        usage = v["usage"].clone();
                    }
                    if let Some(tc) = v.pointer("/choices/0/delta/tool_calls").and_then(Value::as_array) {
                        for c in tc {
                            if c.pointer("/function/name").and_then(Value::as_str).is_some_and(|n| !n.is_empty()) {
                                calls += 1;
                            }
                        }
                    }
                    if let Some(f) = v.pointer("/choices/0/finish_reason").filter(|f| !f.is_null()) {
                        finish = f.clone();
                    }
                }
            }
            if tx.send(Ok(chunk)).await.is_err() {
                break;
            }
        }
        rec["t_end"] = json!(ms(&s2));
        rec["usage"] = usage;
        rec["resp_tool_calls"] = json!(calls);
        rec["finish_reason"] = finish;
        write_log(&s2, &rec);
    });
    let stream = tokio_stream(rx);
    event_stream(Body::from_stream(stream))
}

/// A streamed upstream answer under `--window`: buffered whole, so the
/// provider's prompt count decides between passing it on and rejecting it.
/// Timings are meaningless here; the compaction suite doesn't read them.
async fn windowed(s: Arc<Shared>, resp: reqwest::Response, mut rec: Value) -> Response {
    let bytes = resp.bytes().await.unwrap_or_default();
    rec["t_first_byte"] = json!(ms(&s));
    let text = String::from_utf8_lossy(&bytes);
    let (mut usage, mut calls, mut finish) = (Value::Null, 0usize, Value::Null);
    for line in text.lines() {
        let Some(d) = line.trim().strip_prefix("data:") else { continue };
        let Ok(v) = serde_json::from_str::<Value>(d.trim()) else { continue };
        if v["usage"].is_object() {
            usage = v["usage"].clone();
        }
        if let Some(tc) = v.pointer("/choices/0/delta/tool_calls").and_then(Value::as_array) {
            calls += tc
                .iter()
                .filter(|c| c.pointer("/function/name").and_then(Value::as_str).is_some_and(|n| !n.is_empty()))
                .count();
        }
        if let Some(f) = v.pointer("/choices/0/finish_reason").filter(|f| !f.is_null()) {
            finish = f.clone();
        }
    }
    let prompt = usage["prompt_tokens"].as_u64().unwrap_or(0);
    if let Some(r) = over_window(&s, &mut rec, prompt) {
        return r;
    }
    rec["t_end"] = json!(ms(&s));
    rec["usage"] = usage;
    rec["resp_tool_calls"] = json!(calls);
    rec["finish_reason"] = finish;
    write_log(&s, &rec);
    event_stream(Body::from(bytes))
}
