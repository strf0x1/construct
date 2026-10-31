//! What a provider's server says about its models, by server kind (ADR 0027
//! decision 6): `openai` reads `/models` (vLLM's `max_model_len` ahead of
//! `context_length`), `llama.cpp` adds `/props`' loaded `n_ctx`, `ollama`
//! reads `/api/tags` and each model's `num_ctx` from `/api/show`.
//!
//! The design choice: every parser is a pure function of one response body,
//! pinned by a recorded fixture, and a probe that fails only leaves out what
//! it would have added. A server that is down or of another kind never fails
//! a session; it just states nothing.

use super::Client;
use super::catalog::ModelInfo;
use crate::providers::Discover;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// Ollama's `/api/show` is one request per model; past this many, the rest
/// are listed without a window.
const SHOW_AT_MOST: usize = 50;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The models `client`'s server lists, with whatever windows it states.
pub async fn fetch(client: &Client, how: Discover) -> Result<Vec<ModelInfo>> {
    match how {
        Discover::Off => bail!("discovery is off for this provider"),
        Discover::Openai => client.list_models().await,
        Discover::LlamaCpp => {
            let mut models = client.list_models().await?;
            if let Ok(props) = client.get_root("props").await
                && let Some((n_ctx, alias)) = props_window(&props)
            {
                apply_loaded(&mut models, n_ctx, alias.as_deref());
            }
            Ok(models)
        }
        Discover::Ollama => {
            let tags = client.get_root("api/tags").await?;
            let mut models: Vec<ModelInfo> = tag_names(&tags)
                .into_iter()
                .map(|id| ModelInfo { name: id.clone(), id, tools: true, ..ModelInfo::default() })
                .collect();
            for m in models.iter_mut().take(SHOW_AT_MOST) {
                if let Ok(show) = client.post_root("api/show", &json!({ "model": m.id })).await {
                    m.context = show_num_ctx(&show);
                }
            }
            if models.is_empty() {
                bail!("ollama lists no models");
            }
            Ok(models)
        }
    }
}

/// llama.cpp's `/props`: the loaded context per slot, and the loaded
/// model's alias when it says.
pub fn props_window(props: &Value) -> Option<(u32, Option<String>)> {
    let n = props.pointer("/default_generation_settings/n_ctx").or_else(|| props.get("n_ctx"))?.as_u64()?;
    let alias = props.get("model_alias").and_then(Value::as_str).map(String::from);
    Some((u32::try_from(n).ok().filter(|n| *n > 0)?, alias))
}

/// The loaded `n_ctx` caps the window of the model it belongs to: the only
/// one listed, or the one whose id is the loaded alias. A router in front
/// of llama.cpp lists several under other names; then `/props` is about a
/// model it can't name, and nothing changes.
pub fn apply_loaded(models: &mut [ModelInfo], n_ctx: u32, alias: Option<&str>) {
    let only = models.len() == 1;
    for m in models.iter_mut().filter(|m| only || Some(m.id.as_str()) == alias) {
        m.context = Some(m.context.map_or(n_ctx, |c| c.min(n_ctx)));
    }
}

/// Ollama's `/api/tags`: the model names.
pub fn tag_names(tags: &Value) -> Vec<String> {
    let list = tags.get("models").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    list.iter().filter_map(|m| m.get("name").or_else(|| m.get("model"))?.as_str().map(String::from)).collect()
}

/// Ollama's `/api/show`: `num_ctx` from the model's parameters, the window
/// it runs with. The architecture's trained length is not used: Ollama
/// loads far less unless told to, and planning against more overflows.
pub fn show_num_ctx(show: &Value) -> Option<u32> {
    let params = show.get("parameters")?.as_str()?;
    params.lines().find_map(|l| {
        let mut words = l.split_whitespace();
        (words.next() == Some("num_ctx")).then(|| words.next()?.parse().ok()).flatten()
    })
}

impl Client {
    /// The server's root: `base_url` without its trailing `/v1`.
    fn root(&self) -> &str {
        self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url)
    }

    async fn get_root(&self, path: &str) -> Result<Value> {
        let rb = self.http.get(format!("{}/{path}", self.root())).timeout(PROBE_TIMEOUT);
        self.json(rb, path).await
    }

    async fn post_root(&self, path: &str, body: &Value) -> Result<Value> {
        let rb = self.http.post(format!("{}/{path}", self.root())).json(body).timeout(PROBE_TIMEOUT);
        self.json(rb, path).await
    }

    async fn json(&self, mut rb: reqwest::RequestBuilder, path: &str) -> Result<Value> {
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb.send().await.with_context(|| format!("{path} request failed"))?;
        if !resp.status().is_success() {
            bail!("{path} returned {}", resp.status());
        }
        resp.json().await.with_context(|| format!("{path} is not JSON"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::catalog;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/discover/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn llama_cpp_models_state_max_model_len_and_props_the_loaded_context() {
        let models = catalog::parse(&fixture("llamacpp-models.json"));
        let strata = catalog::find(&models, "strata-flash-next").unwrap();
        assert_eq!(strata.context, Some(262_144), "max_model_len");
        assert_eq!(
            props_window(&fixture("llamacpp-props.json")),
            Some((262_144, Some("qwen3.8-flash-next-iq3_s".into())))
        );
    }

    #[test]
    fn the_loaded_context_caps_only_a_model_it_can_name() {
        let info = |id: &str, c: u32| ModelInfo { id: id.into(), context: Some(c), ..ModelInfo::default() };
        let mut one = vec![info("only", 131_072)];
        apply_loaded(&mut one, 8192, Some("some-file-alias"));
        assert_eq!(one[0].context, Some(8192), "a single listed model is the loaded one");
        let mut routed = vec![info("a", 131_072), info("b", 131_072)];
        apply_loaded(&mut routed, 8192, Some("b"));
        assert_eq!((routed[0].context, routed[1].context), (Some(131_072), Some(8192)));
        let mut unnamed = vec![info("a", 131_072), info("b", 131_072)];
        apply_loaded(&mut unnamed, 8192, Some("qwen3.8-flash-next-iq3_s"));
        assert!(unnamed.iter().all(|m| m.context == Some(131_072)), "a router's alias names neither");
    }

    #[test]
    fn vllm_states_max_model_len_ahead_of_context_length() {
        let models = catalog::parse(&fixture("vllm-models.json"));
        assert_eq!(models[0].context, Some(32_768));
    }

    #[test]
    fn ollama_lists_tags_and_shows_num_ctx() {
        assert_eq!(tag_names(&fixture("ollama-tags.json")), vec!["qwen3:8b", "llama3.2:latest"]);
        assert_eq!(show_num_ctx(&fixture("ollama-show.json")), Some(16_384));
        assert_eq!(show_num_ctx(&json!({ "parameters": "temperature 0.6" })), None, "no num_ctx: unknown");
        assert_eq!(show_num_ctx(&json!({})), None);
    }
}
