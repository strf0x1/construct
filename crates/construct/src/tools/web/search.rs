//! `web_search` backends. One `async fn` per vendor turns the query into that
//! vendor's request and its answer into `Hit`s; a pure `parse_*` beside it does
//! the mapping so it can be tested without a network. The design choice:
//! the backend is the first key that resolves (Exa, Brave, Tavily), else the
//! keyless scrape, and a failing key is reported, never skipped, so a spent
//! quota is not silently answered by a vendor with a different privacy posture.

use super::{ddg::strip_tags, filter::Filter, http::send_json};
use crate::secrets::{BRAVE_API_KEY, EXA_API_KEY, TAVILY_API_KEY};
use crate::tools::Ctx;
use serde_json::{Value, json};
use std::time::Duration;

/// Which keys to suggest when the keyless fallback is rate limited.
pub(super) fn key_names() -> String {
    format!("{EXA_API_KEY}, {BRAVE_API_KEY} or {TAVILY_API_KEY}")
}

/// How much of a snippet the model sees: the best hits get room, the tail a
/// line. Exa is asked for the larger size, since it cannot vary per result.
const TOP_RESULTS: usize = 5;
const TOP_CHARS: usize = 1200;
const REST_CHARS: usize = 300;
const TIMEOUT: Duration = Duration::from_secs(20);

pub(super) struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// A day (`2026-10-21`) when the backend knows one, else its own wording.
    pub date: Option<String>,
}

/// What a backend returns: who answered, the hits, and anything the model
/// should know about how its arguments were handled.
pub(super) struct Answer {
    pub backend: &'static str,
    pub hits: Vec<Hit>,
    pub notes: Vec<String>,
}

pub(super) async fn run(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let q = a.get("query").and_then(Value::as_str).ok_or("query is required")?;
    let n = a.get("limit").and_then(Value::as_u64).unwrap_or(8).clamp(1, 20) as usize;
    let f = Filter::from_args(a);
    let answer = if let Some(key) = crate::secrets::get(EXA_API_KEY) {
        exa(ctx, &key, q, n, &f).await?
    } else if let Some(key) = crate::secrets::get(BRAVE_API_KEY) {
        brave(ctx, &key, q, n, &f).await?
    } else if let Some(key) = crate::secrets::get(TAVILY_API_KEY) {
        tavily(ctx, &key, q, n, &f).await?
    } else {
        super::ddg::search(ctx, q, n, &f).await?
    };
    Ok(render(&answer))
}

fn render(a: &Answer) -> String {
    if a.hits.is_empty() {
        return format!("no results (via {})", a.backend);
    }
    let mut out = vec![format!("via {}", a.backend)];
    out.extend(a.notes.iter().map(|n| format!("note: {n}")));
    for (i, h) in a.hits.iter().enumerate() {
        let max = if i < TOP_RESULTS { TOP_CHARS } else { REST_CHARS };
        let date = h.date.as_deref().map(|d| format!("  ({d})")).unwrap_or_default();
        out.push(format!("{}. {}\n   {}{date}\n   {}", i + 1, h.title, h.url, crate::llm::truncate(&h.snippet, max)));
    }
    out.join("\n")
}

/// `2026-10-21T14:03:00Z` -> `2026-10-21`; anything else (Brave's "3 days
/// ago") is kept as the backend wrote it.
fn day(s: &str) -> Option<String> {
    let s = s.trim();
    let is_iso = s.len() >= 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s[..4].bytes().all(|b| b.is_ascii_digit());
    match (s.is_empty(), is_iso) {
        (true, _) => None,
        (false, true) => Some(s[..10].to_string()),
        (false, false) => Some(s.to_string()),
    }
}

fn text(x: &Value, k: &str) -> String {
    x[k].as_str().unwrap_or("").to_string()
}

async fn exa(ctx: &Ctx, key: &str, q: &str, n: usize, f: &Filter) -> Result<Answer, String> {
    let mut body = json!({ "query": q, "numResults": n, "contents": { "highlights": { "maxCharacters": TOP_CHARS } } });
    if let Some(r) = f.recency {
        body["startPublishedDate"] = r.since().into();
    }
    if !f.domains.is_empty() {
        body["includeDomains"] = json!(f.domains);
    }
    let req = ctx.http.post("https://api.exa.ai/search").header("x-api-key", key).json(&body).timeout(TIMEOUT);
    let v = send_json("Exa", EXA_API_KEY, req).await?;
    Ok(Answer { backend: "Exa", hits: parse_exa(&v), notes: vec![] })
}

fn parse_exa(v: &Value) -> Vec<Hit> {
    v["results"]
        .as_array()
        .map(|r| {
            r.iter()
                .map(|x| {
                    let snippet = x["highlights"]
                        .as_array()
                        .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                        .unwrap_or_default();
                    Hit { title: text(x, "title"), url: text(x, "url"), snippet, date: day(&text(x, "publishedDate")) }
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn brave(ctx: &Ctx, key: &str, q: &str, n: usize, f: &Filter) -> Result<Answer, String> {
    let (q, note) = f.site_query(q);
    let mut params = vec![("q", q), ("count", n.to_string()), ("extra_snippets", "true".into())];
    if let Some(r) = f.recency {
        params.push(("freshness", r.brave().into()));
    }
    let req = ctx
        .http
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", key)
        .query(&params)
        .timeout(TIMEOUT);
    let v = send_json("Brave", BRAVE_API_KEY, req).await?;
    Ok(Answer { backend: "Brave", hits: parse_brave(&v), notes: note.into_iter().collect() })
}

fn parse_brave(v: &Value) -> Vec<Hit> {
    v.pointer("/web/results")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .map(|x| {
                    let mut parts = vec![strip_tags(x["description"].as_str().unwrap_or(""))];
                    parts.extend(
                        x["extra_snippets"].as_array().into_iter().flatten().filter_map(Value::as_str).map(strip_tags),
                    );
                    parts.retain(|p| !p.is_empty());
                    Hit {
                        title: strip_tags(x["title"].as_str().unwrap_or("")),
                        url: text(x, "url"),
                        snippet: parts.join(" "),
                        date: day(&text(x, "page_age")).or_else(|| day(&text(x, "age"))),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn tavily(ctx: &Ctx, key: &str, q: &str, n: usize, f: &Filter) -> Result<Answer, String> {
    let mut body = json!({ "query": q, "max_results": n, "search_depth": "basic" });
    if let Some(r) = f.recency {
        body["time_range"] = r.name().into();
    }
    if !f.domains.is_empty() {
        body["include_domains"] = json!(f.domains);
    }
    let req = ctx.http.post("https://api.tavily.com/search").bearer_auth(key).json(&body).timeout(TIMEOUT);
    let v = send_json("Tavily", TAVILY_API_KEY, req).await?;
    Ok(Answer { backend: "Tavily", hits: parse_tavily(&v), notes: vec![] })
}

fn parse_tavily(v: &Value) -> Vec<Hit> {
    v["results"]
        .as_array()
        .map(|r| {
            r.iter()
                .map(|x| Hit {
                    title: text(x, "title"),
                    url: text(x, "url"),
                    snippet: text(x, "content"),
                    date: day(&text(x, "published_date")),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(snippet: &str, date: Option<&str>) -> Hit {
        Hit { title: "T".into(), url: "https://u".into(), snippet: snippet.into(), date: date.map(Into::into) }
    }

    fn answer(hits: Vec<Hit>) -> Answer {
        Answer { backend: "Exa", hits, notes: vec![] }
    }

    #[test]
    fn exa_joins_highlights_and_keeps_the_day() {
        let v = json!({ "results": [
            { "title": "T", "url": "https://a", "highlights": ["one", "two"], "publishedDate": "2026-10-21T14:03:00.000Z" },
            { "title": "U", "url": "https://b" },
        ]});
        let hits = parse_exa(&v);
        assert_eq!((hits[0].snippet.as_str(), hits[0].date.as_deref()), ("one two", Some("2026-10-21")));
        assert_eq!((hits[1].snippet.as_str(), hits[1].date.as_deref()), ("", None));
    }

    #[test]
    fn brave_strips_markup_and_adds_the_extra_snippets() {
        let v = json!({ "web": { "results": [
            { "title": "<strong>Rust</strong> book", "url": "https://r", "description": "the <strong>book</strong>  of Rust",
              "extra_snippets": ["more", "<strong>again</strong>"], "age": "3 days ago" },
            { "title": "P", "url": "https://p", "description": "", "page_age": "2026-10-01T00:00:00" },
        ]}});
        let hits = parse_brave(&v);
        assert_eq!(hits[0].title, "Rust book");
        assert_eq!(hits[0].snippet, "the book of Rust more again");
        assert_eq!(hits[0].date.as_deref(), Some("3 days ago"));
        assert_eq!((hits[1].snippet.as_str(), hits[1].date.as_deref()), ("", Some("2026-10-01")));
    }

    #[test]
    fn tavily_reads_content_as_the_snippet() {
        let v = json!({ "results": [{ "title": "T", "url": "https://t", "content": "body", "score": 0.9, "published_date": "2026-10-02" }], "usage": {} });
        let hits = parse_tavily(&v);
        assert_eq!((hits[0].snippet.as_str(), hits[0].date.as_deref()), ("body", Some("2026-10-02")));
    }

    #[test]
    fn an_answer_without_results_is_no_results_from_that_backend() {
        for v in [json!({}), json!({ "results": [] }), json!({ "web": {} })] {
            assert!(parse_exa(&v).is_empty() && parse_brave(&v).is_empty() && parse_tavily(&v).is_empty());
        }
        assert_eq!(render(&answer(vec![])), "no results (via Exa)");
    }

    #[test]
    fn the_answer_names_its_backend_notes_and_dates() {
        let mut a = answer(vec![hit("snip", Some("2026-10-21"))]);
        a.notes.push("recency was ignored".into());
        assert_eq!(render(&a), "via Exa\nnote: recency was ignored\n1. T\n   https://u  (2026-10-21)\n   snip");
    }

    #[test]
    fn the_top_results_get_more_room_than_the_rest() {
        let long = "x".repeat(2000);
        let out = render(&answer((0..7).map(|_| hit(&long, None)).collect()));
        let snippets: Vec<usize> = out.lines().filter(|l| l.starts_with("   x")).map(|l| l.len()).collect();
        assert_eq!(snippets.len(), 7);
        assert!(snippets[..5].iter().all(|n| (TOP_CHARS..TOP_CHARS + 10).contains(n)), "{snippets:?}");
        assert!(snippets[5..].iter().all(|n| (REST_CHARS..REST_CHARS + 10).contains(n)), "{snippets:?}");
    }

    #[test]
    fn only_a_day_shaped_date_is_cut() {
        assert_eq!(day("2026-10-21T14:03:00Z").as_deref(), Some("2026-10-21"));
        assert_eq!(day("2 days ago").as_deref(), Some("2 days ago"));
        assert_eq!(day("  "), None);
    }
}
