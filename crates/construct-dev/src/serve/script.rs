//! The script vocabulary: what a scripted model answers with. A step is one
//! reply of the agent loop's; a side reply answers a call without tools (a
//! summarizer, a title), and is plain text unless it needs a step's knobs.
//! `adapt` turns a step's abstract tool name into whichever tool the harness
//! offers, so one script drives construct, Hermes and opencode.

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Step {
    Text {
        text: String,
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        reasoning: Option<String>,
        /// Unparseable `data:` lines sent before the words, the way a flaky
        /// proxy garbles a stream.
        #[serde(default)]
        bad_chunks: usize,
        /// End the stream after the words: no finish reason, usage or `[DONE]`.
        #[serde(default)]
        cut: bool,
        /// Finish with `length`, as a model stopped by its output limit.
        #[serde(default)]
        length: bool,
        /// Go silent this long after the words, before the finish: a
        /// provider that stalls mid-stream.
        #[serde(default)]
        stall_ms: u64,
    },
    Call {
        call: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        delay_ms: u64,
        /// Thinking streamed before the call, as `reasoning_content`.
        #[serde(default)]
        reasoning: Option<String>,
    },
    Calls {
        calls: Vec<Value>,
    },
    /// Answer with this HTTP status instead of a stream: 429 and 5xx are
    /// retried by clients, 400 is a provider rejecting a parameter.
    Fail {
        status: u16,
        #[serde(default)]
        retry_after: Option<u64>,
        #[serde(default)]
        body: String,
    },
}

/// A reply to a side call: plain text, or a step for anything more.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Side {
    Text(String),
    Step(Step),
}

/// Pick the harness's tool matching an abstract name, and its argument names.
pub(super) fn adapt(req: &Value, call: &str, args: &Value) -> (String, Value) {
    let tools = req["tools"].as_array().cloned().unwrap_or_default();
    let names: &[&str] = match call {
        "read" => &["read", "read_file", "Read", "view"],
        "shell" => &["shell", "bash", "terminal", "Bash", "run_shell_command"],
        "write" => &["write", "write_file", "Write"],
        "edit" => &["edit", "patch", "Edit", "replace"],
        "grep" => &["grep", "search", "search_files", "Grep"],
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
