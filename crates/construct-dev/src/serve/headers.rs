//! The recording proxy's header boundary: explicit routing hints, never caller credentials.

use axum::http::HeaderMap;
use reqwest::RequestBuilder;
use serde_json::{Value, json};

pub(super) const ORCA_REQUEST_ID: &str = "x-orca-request-id";

pub(super) fn is_orca(upstream: &str) -> bool {
    reqwest::Url::parse(upstream).is_ok_and(|url| url.host_str() == Some("api.orcarouter.ai"))
}

pub(super) fn forward(headers: &HeaderMap, mut request: RequestBuilder, upstream: &str) -> RequestBuilder {
    if !headers.contains_key("x-orcarouter-include-cost") && is_orca(upstream) {
        request = request.header("x-orcarouter-include-cost", "true");
    }
    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "http-referer" | "x-title" | "user-agent" | "x-orcarouter-session-id" | "x-orcarouter-include-cost"
        ) {
            request = request.header(name, value);
        }
    }
    request
}

pub(super) fn record(headers: &HeaderMap, record: &mut Value) {
    for (header, field) in [(ORCA_REQUEST_ID, "upstream_request_id"), ("x-orca-resolved-model", "resolved_model")] {
        if let Some(value) = headers.get(header).and_then(|v| v.to_str().ok()) {
            record[field] = json!(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_opt_in_is_added_only_for_the_exact_orca_host() {
        for (url, expected) in [
            ("https://api.orcarouter.ai/v1", true),
            ("https://api.orcarouter.ai.other.example/v1", false),
            ("https://openrouter.ai/api/v1", false),
        ] {
            let client = construct_core::http::builder().build().unwrap();
            let request = forward(&HeaderMap::new(), client.post(url), url).build().unwrap();
            assert_eq!(request.headers().contains_key("x-orcarouter-include-cost"), expected, "{url}");
        }
    }
}
