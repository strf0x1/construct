//! `GET` on the scripted endpoint: the model list. Scripted, it is two
//! models in OpenRouter's shape (one with a small window and prices);
//! with `upstream`, the provider's own list, passed through. `models_ms`
//! delays it, as a slow server would.

use super::{Shared, auth};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

pub(super) async fn list(State(s): State<Arc<Shared>>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    tokio::time::sleep(std::time::Duration::from_millis(s.cfg.models_ms)).await;
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
    axum::Json(json!({ "object": "list", "data": [
        { "id": "mock", "object": "model", "context_length": 131072 },
        // OpenRouter's shape: a small window, per-token prices as strings.
        { "id": "mock/small", "name": "Mock: Small", "context_length": 32768,
          "pricing": { "prompt": "0.0000003", "completion": "0.0000025" },
          "supported_parameters": ["tools", "tool_choice"] },
    ] }))
    .into_response()
}
