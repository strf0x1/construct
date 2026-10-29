//! One JSON request to a search API, with the failures a model can act on.
//! Every backend goes through `send_json`, so a bad key, a spent quota or a
//! rate limit reads as itself instead of as "no results". The design choice:
//! a 429 is retried once, and only when the server asks for a short wait;
//! anything longer is reported with the wait so the model can decide.

use reqwest::{RequestBuilder, header::RETRY_AFTER};
use serde_json::Value;
use std::time::Duration;

/// The longest `Retry-After` worth sleeping through inside one tool call.
const MAX_RETRY_WAIT: u64 = 5;

/// Send `req` and parse the JSON body. `name` is the vendor as the model
/// should read it, `key` the secret to point the user at on a rejected key.
pub(super) async fn send_json(name: &str, key: &str, req: RequestBuilder) -> Result<Value, String> {
    let retry = req.try_clone();
    let mut resp = req.send().await.map_err(|e| format!("{name} request failed: {e}"))?;
    if resp.status().as_u16() == 429
        && let Some(retry) = retry
        && let Some(wait) = retry_after(&resp).filter(|w| *w <= MAX_RETRY_WAIT)
    {
        tokio::time::sleep(Duration::from_secs(wait.max(1))).await;
        resp = retry.send().await.map_err(|e| format!("{name} request failed: {e}"))?;
    }
    let status = resp.status().as_u16();
    let wait = retry_after(&resp);
    let body = resp.text().await.map_err(|e| format!("{name} response unreadable: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(http_error(name, key, status, wait, &body));
    }
    serde_json::from_str(&body).map_err(|e| format!("{name} returned something that is not JSON: {e}"))
}

fn retry_after(resp: &reqwest::Response) -> Option<u64> {
    resp.headers().get(RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()
}

/// Prose for a non-2xx answer. 432 and 433 are Tavily's key-limit and
/// pay-as-you-go-limit codes; 402 is the usual "out of credit".
fn http_error(name: &str, key: &str, status: u16, retry_after: Option<u64>, body: &str) -> String {
    match status {
        401 | 403 => format!("{name} rejected the key (HTTP {status}); check {key} with `construct secrets set {key}`"),
        402 | 432 | 433 => format!("{name} says the plan or credit limit is used up (HTTP {status}){}", detail(body)),
        429 => match retry_after {
            Some(w) => format!("{name} rate limited this request; retry in {w}s"),
            None => format!("{name} rate limited this request; wait a moment and retry"),
        },
        _ => format!("{name} returned HTTP {status}{}", detail(body)),
    }
}

/// `: <message>` from the common error shapes (`error`, `message`, Tavily's
/// `detail.error`), else a short slice of the raw body, else nothing.
fn detail(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let text = [&v["error"], &v["message"], &v["detail"]]
        .into_iter()
        .find_map(|f| match f {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("error").or(o.get("message")).and_then(Value::as_str).map(str::to_string),
            _ => None,
        })
        .unwrap_or_else(|| body.trim().to_string());
    if text.is_empty() { String::new() } else { format!(": {}", crate::llm::truncate(&text, 300)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_key_names_the_secret_to_fix() {
        let m = http_error("Exa", "EXA_API_KEY", 401, None, r#"{"error":"invalid api key"}"#);
        assert!(m.contains("rejected the key") && m.contains("EXA_API_KEY"), "{m}");
    }

    #[test]
    fn spent_quota_is_not_reported_as_empty_results() {
        for status in [402, 432, 433] {
            let m = http_error("Tavily", "TAVILY_API_KEY", status, None, r#"{"detail":{"error":"Usage limit"}}"#);
            assert!(m.contains("limit is used up") && m.contains("Usage limit"), "{m}");
        }
    }

    #[test]
    fn a_rate_limit_says_how_long_to_wait() {
        assert!(http_error("Brave", "BRAVE_API_KEY", 429, Some(30), "").contains("retry in 30s"));
        assert!(http_error("Brave", "BRAVE_API_KEY", 429, None, "").contains("wait a moment"));
    }

    #[test]
    fn other_errors_carry_the_vendors_message() {
        assert_eq!(http_error("Exa", "EXA_API_KEY", 500, None, r#"{"message":"boom"}"#), "Exa returned HTTP 500: boom");
        assert_eq!(http_error("Exa", "EXA_API_KEY", 502, None, ""), "Exa returned HTTP 502");
        assert!(http_error("Exa", "EXA_API_KEY", 400, None, "<html>bad</html>").ends_with(": <html>bad</html>"));
    }

    /// A one-connection-per-reply server: each accepted connection gets the
    /// next canned response. Returns the base URL.
    async fn serve(replies: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            for reply in replies {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                sock.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        url
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn a_bad_key_is_an_error_not_an_empty_answer() {
        let url = serve(vec![reply("401 Unauthorized", "", r#"{"error":"nope"}"#)]).await;
        let err = send_json("Exa", "EXA_API_KEY", reqwest::Client::new().get(&url)).await.unwrap_err();
        assert!(err.contains("rejected the key"), "{err}");
    }

    #[tokio::test]
    async fn a_short_rate_limit_is_retried_once() {
        let url = serve(vec![
            reply("429 Too Many Requests", "retry-after: 1\r\n", ""),
            reply("200 OK", "", r#"{"ok":true}"#),
        ])
        .await;
        let v = send_json("Tavily", "TAVILY_API_KEY", reqwest::Client::new().get(&url)).await.unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn a_long_rate_limit_is_reported_without_waiting() {
        let url = serve(vec![reply("429 Too Many Requests", "retry-after: 60\r\n", "")]).await;
        let err = send_json("Brave", "BRAVE_API_KEY", reqwest::Client::new().get(&url)).await.unwrap_err();
        assert!(err.contains("retry in 60s"), "{err}");
    }
}
