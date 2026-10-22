//! JSON-RPC 2.0 as ACP uses it: one JSON object per line, in both directions.
//! Pure functions from values to lines and lines to [`Incoming`]; the server
//! and the client own their I/O and only share this.
//!
//! The design choice: ids are opaque `Value`s on the way in (a peer may use
//! numbers or strings, and a response must echo it exactly), and plain `u64`s
//! on the way out, which is all construct ever sends.

use serde_json::{Value, json};

/// Standard JSON-RPC error codes ACP uses.
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL: i64 = -32603;

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
    Response { id: Value, result: Result<Value, RpcError> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// One line from the peer, or `None` for anything that isn't a JSON-RPC
/// message (blank lines, stray output): a reader skips those.
pub fn parse(line: &str) -> Option<Incoming> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let obj = v.as_object()?;
    let method = obj.get("method").and_then(Value::as_str).map(str::to_string);
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    match (method, obj.get("id")) {
        (Some(method), Some(id)) => Some(Incoming::Request { id: id.clone(), method, params }),
        (Some(method), None) => Some(Incoming::Notification { method, params }),
        (None, Some(id)) => {
            let result = match obj.get("error") {
                Some(e) => Err(RpcError {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(INTERNAL),
                    message: error_text(e),
                }),
                None => Ok(obj.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id: id.clone(), result })
        }
        (None, None) => None,
    }
}

/// An error's message with the reason an agent put in `data`, which is where
/// "Internal error" gets explained.
fn error_text(e: &Value) -> String {
    let message = e.get("message").and_then(Value::as_str).unwrap_or("");
    let data = match e.get("data") {
        Some(Value::String(s)) => s.clone(),
        Some(d) => d.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| d.to_string()),
        None => String::new(),
    };
    match (message.is_empty(), data.is_empty() || data == "null") {
        (_, true) => message.to_string(),
        (true, false) => data,
        (false, false) => format!("{message}: {data}"),
    }
}

fn line(v: Value) -> String {
    format!("{v}\n")
}

pub fn request(id: u64, method: &str, params: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

pub fn notification(method: &str, params: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

pub fn ok(id: &Value, result: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

pub fn err(id: &Value, code: i64, message: &str) -> String {
    line(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_message_kinds_parse_and_round_trip() {
        let req = parse(&request(7, "session/new", json!({"cwd": "/w"}))).unwrap();
        assert_eq!(req, Incoming::Request { id: json!(7), method: "session/new".into(), params: json!({"cwd": "/w"}) });
        let note = parse(&notification("session/cancel", json!({"sessionId": "s"}))).unwrap();
        assert!(matches!(note, Incoming::Notification { method, .. } if method == "session/cancel"));
        // A string id is echoed as it came.
        let resp = parse(&ok(&json!("abc"), json!({"x": 1}))).unwrap();
        assert_eq!(resp, Incoming::Response { id: json!("abc"), result: Ok(json!({"x": 1})) });
        let bad = parse(&err(&json!(1), METHOD_NOT_FOUND, "no")).unwrap();
        assert_eq!(
            bad,
            Incoming::Response { id: json!(1), result: Err(RpcError { code: METHOD_NOT_FOUND, message: "no".into() }) }
        );
    }

    #[test]
    fn an_errors_data_explains_it() {
        let line = r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"Internal error","data":{"message":"model not supported"}}}"#;
        let Some(Incoming::Response { result: Err(e), .. }) = parse(line) else { panic!("not an error") };
        assert_eq!(e.message, "Internal error: model not supported");
    }

    #[test]
    fn noise_is_skipped() {
        for l in ["", "   ", "not json", "[1,2]", "{}", "{\"jsonrpc\":\"2.0\"}"] {
            assert!(parse(l).is_none(), "{l:?}");
        }
    }
}
