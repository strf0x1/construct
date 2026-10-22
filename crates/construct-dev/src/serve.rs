//! An OpenAI-compatible endpoint for tests and benches.
//!
//! Script mode answers from a list of steps, adapting tool calls to whatever
//! tools the harness offers (`read` becomes read_file/read/view with the right
//! path parameter, `shell` becomes bash/terminal/shell), so the same script
//! drives construct, Hermes and opencode.
//!
//! Upstream mode forwards to a real endpoint, injects the key (so harness
//! configs never hold it) and asks for usage on every stream.
//!
//! With `cache`, script mode also acts as a provider with an automatic prefix
//! cache (tools, then each message, the order providers cache in), so cache
//! hit rates and a prefix that breaks between requests show up without a real
//! model. It is off by default: re-serializing each request is time the bench
//! would bill to the harness.
//!
//! Both modes log every request with timestamps to JSONL, which is what the
//! bench reads: tool-call latency is the gap between a response that ended in
//! tool calls and the harness's next request.

use anyhow::Result;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Step {
    Text {
        text: String,
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        reasoning: Option<String>,
    },
    Call {
        call: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        delay_ms: u64,
    },
    Calls {
        calls: Vec<Value>,
    },
}

pub struct Config {
    pub script: Vec<Step>,
    /// After the script: answer N more requests with this tool call before stopping.
    pub repeat_call: Option<(Step, usize)>,
    /// Replies to side calls (no tools), in order; "Side reply" after that.
    pub side: Vec<String>,
    pub upstream: Option<String>,
    pub key: Option<String>,
    pub log: Option<PathBuf>,
    pub dump_dir: Option<PathBuf>,
    pub chunk_ms: u64,
    /// Report simulated prefix-cache hits and writes in usage.
    pub cache: bool,
}

struct Shared {
    cfg: Config,
    next: Mutex<usize>,
    side_next: Mutex<usize>,
    repeated: Mutex<usize>,
    start: Instant,
    log: Mutex<Option<std::fs::File>>,
    count: Mutex<usize>,
    http: reqwest::Client,
    /// Hashes of every request prefix seen, for the simulated cache.
    prefixes: Mutex<HashSet<u64>>,
}

pub async fn serve(cfg: Config, port: u16) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let log = cfg.log.as_ref().map(|p| std::fs::OpenOptions::new().create(true).append(true).open(p)).transpose()?;
    if let Some(d) = &cfg.dump_dir {
        std::fs::create_dir_all(d)?;
    }
    let shared = Arc::new(Shared {
        cfg,
        next: Mutex::new(0),
        side_next: Mutex::new(0),
        repeated: Mutex::new(0),
        start: Instant::now(),
        log: Mutex::new(log),
        count: Mutex::new(0),
        prefixes: Mutex::new(HashSet::new()),
        http: reqwest::Client::builder().pool_idle_timeout(Duration::from_secs(120)).build()?,
    });
    let app = Router::new().route("/{*path}", post(chat)).route("/{*path}", get(models)).with_state(shared);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let port = listener.local_addr()?.port();
    // Without TCP_NODELAY the tail of a streamed response can wait ~40ms on
    // delayed ACKs, which would be billed to the harness under test.
    use axum::serve::ListenerExt;
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    let h = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((port, h))
}

async fn models(State(s): State<Arc<Shared>>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    if let Some(up) = &s.cfg.upstream {
        let path = uri.path().trim_start_matches("/v1").trim_start_matches("/api/v1");
        let mut rb = s.http.get(format!("{up}{path}"));
        if let Some(k) = auth(&s, &headers) {
            rb = rb.bearer_auth(k);
        }
        return match rb.send().await {
            Ok(r) => {
                let st = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::OK);
                (st, r.bytes().await.unwrap_or_default()).into_response()
            }
            Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        };
    }
    axum::Json(json!({ "object": "list", "data": [{ "id": "mock", "object": "model", "context_length": 131072 }] }))
        .into_response()
}

fn auth(s: &Shared, headers: &HeaderMap) -> Option<String> {
    s.cfg.key.clone().or_else(|| {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(String::from)
    })
}

fn ms(s: &Shared) -> f64 {
    s.start.elapsed().as_secs_f64() * 1000.0
}

fn summarize(body: &Value) -> Value {
    let tools: Vec<String> = body["tools"]
        .as_array()
        .map(|t| {
            t.iter().filter_map(|x| x.pointer("/function/name").and_then(Value::as_str).map(String::from)).collect()
        })
        .unwrap_or_default();
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    let system_chars: usize = msgs
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| match &m["content"] {
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        })
        .sum();
    json!({
        "model": body["model"],
        "stream": body["stream"],
        "n_messages": msgs.len(),
        "last_role": msgs.last().map(|m| m["role"].clone()).unwrap_or(Value::Null),
        "n_tools": tools.len(),
        "tools": tools,
        "system_chars": system_chars,
        "tools_chars": body["tools"].to_string().len(),
    })
}

async fn chat(State(s): State<Arc<Shared>>, headers: HeaderMap, uri: axum::http::Uri, body: Bytes) -> Response {
    if !uri.path().ends_with("chat/completions") {
        // e.g. Hermes probes Ollama's /api/show on loopback endpoints
        return (StatusCode::NOT_FOUND, axum::Json(json!({ "error": { "message": "not found" } }))).into_response();
    }
    let t_arrive = ms(&s);
    let idx = {
        let mut c = s.count.lock().unwrap();
        *c += 1;
        *c
    };
    let mut req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    if let Some(d) = &s.cfg.dump_dir {
        let _ = std::fs::write(d.join(format!("req-{idx:04}.json")), &body);
    }
    let mut rec = summarize(&req);
    rec["i"] = json!(idx);
    rec["t_arrive"] = json!(t_arrive);
    rec["req_bytes"] = json!(body.len());
    rec["path"] = json!(uri.path());
    if let Some(up) = s.cfg.upstream.clone() {
        return forward(s, up, headers, uri, &mut req, rec).await;
    }
    scripted(s, &req, rec).await
}

fn write_log(s: &Shared, rec: &Value) {
    if let Some(f) = s.log.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{rec}");
        let _ = f.flush();
    }
}

async fn forward(
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
    for (k, v) in headers.iter() {
        let n = k.as_str();
        if n.starts_with("http-referer") || n.starts_with("x-title") || n == "user-agent" {
            rb = rb.header(n, v.clone());
        }
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            rec["error"] = json!(e.to_string());
            write_log(&s, &rec);
            return (StatusCode::BAD_GATEWAY, e.to_string()).into_response();
        }
    };
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
        return Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(bytes))
            .unwrap();
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
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn tokio_stream(
    mut rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Pick the harness's tool matching an abstract name, and its argument names.
fn adapt(req: &Value, call: &str, args: &Value) -> (String, Value) {
    let tools = req["tools"].as_array().cloned().unwrap_or_default();
    let names: &[&str] = match call {
        "read" => &["read", "read_file", "Read", "view"],
        "shell" => &["shell", "bash", "terminal", "Bash", "run_shell_command"],
        "write" => &["write", "write_file", "Write"],
        "edit" => &["edit", "patch", "Edit", "replace"],
        "grep" => &["grep", "search_files", "Grep"],
        other => return (other.to_string(), args.clone()),
    };
    let found =
        tools.iter().find(|t| names.contains(&t.pointer("/function/name").and_then(Value::as_str).unwrap_or("")));
    let Some(tool) = found else { return (names[0].to_string(), args.clone()) };
    let name = tool.pointer("/function/name").and_then(Value::as_str).unwrap_or(names[0]).to_string();
    let props = tool.pointer("/function/parameters/properties").cloned().unwrap_or(json!({}));
    let mut out = serde_json::Map::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            let candidates: Vec<&str> = match k.as_str() {
                "path" => vec!["path", "filePath", "file_path", "filename"],
                "command" => vec!["command", "cmd"],
                "content" => vec!["content", "contents"],
                "pattern" => vec!["pattern", "query", "regex"],
                k => vec![k],
            };
            let key = candidates.iter().find(|c| props.get(**c).is_some()).copied().unwrap_or(candidates[0]);
            out.insert(key.to_string(), v.clone());
        }
    }
    // opencode's bash wants a description; give required strings a value.
    if let Some(req_fields) = tool.pointer("/function/parameters/required").and_then(Value::as_array) {
        for f in req_fields.iter().filter_map(Value::as_str) {
            if !out.contains_key(f) && props[f]["type"] == "string" {
                out.insert(f.to_string(), json!("bench step"));
            }
        }
    }
    (name, Value::Object(out))
}

/// Bytes of the longest prefix of `req` (tools, then messages) that an
/// earlier request already sent; every prefix of this one is cached after.
fn cached_bytes(s: &Shared, req: &Value) -> u64 {
    let mut blocks = vec![req["tools"].to_string()];
    blocks.extend(req["messages"].as_array().into_iter().flatten().map(Value::to_string));
    let mut seen = s.prefixes.lock().unwrap();
    let (mut h, mut len, mut hit, mut missed) = (DefaultHasher::new(), 0u64, 0u64, false);
    for b in &blocks {
        h.write(b.as_bytes());
        len += b.len() as u64;
        if !missed && seen.contains(&h.finish()) {
            hit = len;
        } else {
            missed = true;
            seen.insert(h.finish());
        }
    }
    hit
}

fn sse(v: Value) -> Bytes {
    Bytes::from(format!("data: {v}\n\n"))
}

async fn scripted(s: Arc<Shared>, req: &Value, mut rec: Value) -> Response {
    // Side calls without tools (opencode's title generation, compaction)
    // don't advance the script; only the agent loop does.
    let side = req["tools"].as_array().is_none_or(|t| t.is_empty());
    rec["side"] = json!(side);
    let step = if side {
        let mut n = s.side_next.lock().unwrap();
        let text = s.cfg.side.get(*n).cloned().unwrap_or_else(|| "Side reply".into());
        *n += 1;
        Some(Step::Text { text, delay_ms: 0, reasoning: None })
    } else {
        let mut n = s.next.lock().unwrap();
        let st = s.cfg.script.get(*n).cloned();
        if st.is_some() {
            *n += 1;
        }
        st.or_else(|| {
            let (st, max) = s.cfg.repeat_call.clone()?;
            let mut r = s.repeated.lock().unwrap();
            (*r < max).then(|| {
                *r += 1;
                st
            })
        })
    };
    let step = match step {
        Some(st) => st,
        None => Step::Text { text: "done".into(), delay_ms: 0, reasoning: None },
    };
    let model = req["model"].as_str().unwrap_or("mock").to_string();
    let stream = req["stream"].as_bool().unwrap_or(false);
    let base = |delta: Value, finish: Value| json!({ "id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] });
    let prompt_tokens = rec["req_bytes"].as_u64().unwrap_or(0) / 4;
    let mut usage =
        json!({ "prompt_tokens": prompt_tokens, "completion_tokens": 10, "total_tokens": prompt_tokens + 10 });
    if s.cfg.cache {
        let cached = (cached_bytes(&s, req) / 4).min(prompt_tokens);
        // Only providers that cache on request report writes.
        let written = if req.to_string().contains("\"cache_control\"") { prompt_tokens - cached } else { 0 };
        usage["prompt_tokens_details"] = json!({ "cached_tokens": cached, "cache_write_tokens": written });
    }
    let mut chunks: Vec<(u64, Bytes)> = vec![];
    let mut message = json!({ "role": "assistant", "content": Value::Null });
    let mut n_calls = 0;
    match &step {
        Step::Text { text, delay_ms, reasoning } => {
            let mut first = *delay_ms;
            if let Some(r) = reasoning {
                chunks.push((first, sse(base(json!({ "role": "assistant", "reasoning_content": r }), Value::Null))));
                first = 0;
            }
            // stream words so the UI sees real deltas
            let words: Vec<&str> = text.split_inclusive(' ').collect();
            for (i, w) in words.iter().enumerate() {
                let d = if i == 0 { first } else { s.cfg.chunk_ms };
                chunks.push((d, sse(base(json!({ "content": w }), Value::Null))));
            }
            chunks.push((0, sse(base(json!({}), json!("stop")))));
            message["content"] = json!(text);
        }
        Step::Call { call, args, delay_ms } => {
            let (name, a) = adapt(req, call, args);
            n_calls = 1;
            let tc = json!([{ "index": 0, "id": format!("call_{}", rec["i"]), "type": "function", "function": { "name": name, "arguments": a.to_string() } }]);
            chunks.push((*delay_ms, sse(base(json!({ "role": "assistant", "tool_calls": tc }), Value::Null))));
            chunks.push((0, sse(base(json!({}), json!("tool_calls")))));
            message["tool_calls"] = tc;
        }
        Step::Calls { calls } => {
            let mut tcs = vec![];
            for (i, c) in calls.iter().enumerate() {
                let (name, a) = adapt(req, c["call"].as_str().unwrap_or(""), &c["args"]);
                tcs.push(json!({ "index": i, "id": format!("call_{}_{i}", rec["i"]), "type": "function", "function": { "name": name, "arguments": a.to_string() } }));
            }
            n_calls = tcs.len();
            chunks.push((0, sse(base(json!({ "role": "assistant", "tool_calls": tcs }), Value::Null))));
            chunks.push((0, sse(base(json!({}), json!("tool_calls")))));
            message["tool_calls"] = json!(tcs);
        }
    }
    rec["resp_tool_calls"] = json!(n_calls);
    rec["usage"] = usage.clone();
    if !stream {
        let t = ms(&s);
        rec["t_first_byte"] = json!(t);
        rec["t_end"] = json!(t);
        write_log(&s, &rec);
        let finish = if n_calls > 0 { "tool_calls" } else { "stop" };
        return axum::Json(json!({ "id": "chatcmpl-mock", "object": "chat.completion", "created": 0, "model": model, "choices": [{ "index": 0, "message": message, "finish_reason": finish }], "usage": usage })).into_response();
    }
    chunks.push((0, sse(json!({ "id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [], "usage": usage }))));
    chunks.push((0, Bytes::from_static(b"data: [DONE]\n\n")));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let s2 = s.clone();
    tokio::spawn(async move {
        let mut first = true;
        for (delay, c) in chunks {
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            if first {
                rec["t_first_byte"] = json!(ms(&s2));
                first = false;
            }
            if tx.send(Ok(c)).await.is_err() {
                break;
            }
        }
        rec["t_end"] = json!(ms(&s2));
        write_log(&s2, &rec);
    });
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(tokio_stream(rx)))
        .unwrap()
}
