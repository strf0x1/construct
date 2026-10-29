//! `web_search` backends. One `async fn` per vendor turns the query into that
//! vendor's request and its answer into `Hit`s; a pure `parse_*` beside it does
//! the mapping so it can be tested without a network. The design choice:
//! the backend is the first key that resolves (Exa, Brave, Tavily), else the
//! keyless scrape, and a failing key is reported, never skipped, so a spent
//! quota is not silently answered by a vendor with a different privacy posture.

use super::{ddg::strip_tags, http::send_json};
use crate::secrets::{BRAVE_API_KEY, EXA_API_KEY, TAVILY_API_KEY};
use crate::tools::Ctx;
use serde_json::{Value, json};
use std::time::Duration;

/// Which keys to suggest when the keyless fallback is rate limited.
pub(super) fn key_names() -> String {
    format!("{EXA_API_KEY}, {BRAVE_API_KEY} or {TAVILY_API_KEY}")
}

/// Characters of a result's snippet shown to the model, and asked of Exa.
const SNIPPET_CHARS: usize = 400;
const TIMEOUT: Duration = Duration::from_secs(20);

/// title, url, snippet.
pub(super) type Hit = (String, String, String);

pub(super) async fn run(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let q = a.get("query").and_then(Value::as_str).ok_or("query is required")?;
    let n = a.get("limit").and_then(Value::as_u64).unwrap_or(8).clamp(1, 20) as usize;
    let results = if let Some(key) = crate::secrets::get(EXA_API_KEY) {
        exa(ctx, &key, q, n).await?
    } else if let Some(key) = crate::secrets::get(BRAVE_API_KEY) {
        brave(ctx, &key, q, n).await?
    } else if let Some(key) = crate::secrets::get(TAVILY_API_KEY) {
        tavily(ctx, &key, q, n).await?
    } else {
        super::ddg::search(ctx, q, n).await?
    };
    Ok(render(&results))
}

fn render(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "no results".into();
    }
    hits.iter()
        .enumerate()
        .map(|(i, (t, u, s))| format!("{}. {t}\n   {u}\n   {}", i + 1, crate::llm::truncate(s, SNIPPET_CHARS)))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn exa(ctx: &Ctx, key: &str, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let req = ctx
        .http
        .post("https://api.exa.ai/search")
        .header("x-api-key", key)
        .json(&json!({ "query": q, "numResults": n, "contents": { "highlights": { "maxCharacters": SNIPPET_CHARS } } }))
        .timeout(TIMEOUT);
    Ok(parse_exa(&send_json("Exa", EXA_API_KEY, req).await?))
}

fn parse_exa(v: &Value) -> Vec<Hit> {
    let text = |x: &Value, k: &str| x[k].as_str().unwrap_or("").to_string();
    v["results"]
        .as_array()
        .map(|r| {
            r.iter()
                .map(|x| {
                    let snippet = x["highlights"]
                        .as_array()
                        .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                        .unwrap_or_default();
                    (text(x, "title"), text(x, "url"), snippet)
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn brave(ctx: &Ctx, key: &str, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let req = ctx
        .http
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", key)
        .query(&[("q", q), ("count", &n.to_string())])
        .timeout(TIMEOUT);
    Ok(parse_brave(&send_json("Brave", BRAVE_API_KEY, req).await?))
}

fn parse_brave(v: &Value) -> Vec<Hit> {
    v.pointer("/web/results")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .map(|x| {
                    (
                        strip_tags(x["title"].as_str().unwrap_or("")),
                        x["url"].as_str().unwrap_or("").to_string(),
                        strip_tags(x["description"].as_str().unwrap_or("")),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn tavily(ctx: &Ctx, key: &str, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let req = ctx
        .http
        .post("https://api.tavily.com/search")
        .bearer_auth(key)
        .json(&json!({ "query": q, "max_results": n, "search_depth": "basic" }))
        .timeout(TIMEOUT);
    Ok(parse_tavily(&send_json("Tavily", TAVILY_API_KEY, req).await?))
}

fn parse_tavily(v: &Value) -> Vec<Hit> {
    let text = |x: &Value, k: &str| x[k].as_str().unwrap_or("").to_string();
    v["results"]
        .as_array()
        .map(|r| r.iter().map(|x| (text(x, "title"), text(x, "url"), text(x, "content"))).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exa_joins_highlights() {
        let v = json!({ "results": [
            { "title": "T", "url": "https://a", "highlights": ["one", "two"] },
            { "title": "U", "url": "https://b" },
        ]});
        assert_eq!(
            parse_exa(&v),
            vec![("T".into(), "https://a".into(), "one two".into()), ("U".into(), "https://b".into(), "".into())]
        );
    }

    #[test]
    fn brave_strips_the_markup_in_its_fields() {
        let v = json!({ "web": { "results": [
            { "title": "<strong>Rust</strong> book", "url": "https://r", "description": "the <strong>book</strong>  of Rust" },
        ]}});
        assert_eq!(parse_brave(&v), vec![("Rust book".into(), "https://r".into(), "the book of Rust".into())]);
    }

    #[test]
    fn tavily_reads_content_as_the_snippet() {
        let v =
            json!({ "results": [{ "title": "T", "url": "https://t", "content": "body", "score": 0.9 }], "usage": {} });
        assert_eq!(parse_tavily(&v), vec![("T".into(), "https://t".into(), "body".into())]);
    }

    #[test]
    fn an_answer_without_results_is_no_results() {
        for v in [json!({}), json!({ "results": [] }), json!({ "web": {} })] {
            assert!(parse_exa(&v).is_empty() && parse_brave(&v).is_empty() && parse_tavily(&v).is_empty());
        }
        assert_eq!(render(&[]), "no results");
    }

    #[test]
    fn snippets_are_capped() {
        let out = render(&[("T".into(), "u".into(), "x".repeat(SNIPPET_CHARS * 2))]);
        assert!(out.len() < SNIPPET_CHARS + 40, "{}", out.len());
    }
}
