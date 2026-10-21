//! session_search: the user's past agent sessions, through chatstrata.

use super::{Ctx, Display, Group, Output, Tool, ToolError, def, obj};
use serde_json::{Value, json};

pub struct SessionSearch;

impl SessionSearch {
    pub const NAME: &str = "session_search";
}

#[async_trait::async_trait]
impl Tool for SessionSearch {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::History
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Search the user's past agent sessions (all harnesses, via chatstrata) by keywords. Use when the user refers to earlier work or you need a past decision.",
            obj(
                json!({
                    "query": { "type": "string" },
                    "limit": { "type": "integer" },
                    "since": { "type": "string", "description": "YYYY-MM-DD" }
                }),
                &["query"],
            ),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "query", ..Display::default() }
    }
    async fn call(&self, _ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        run(&a).await.map(Output::plain).map_err(ToolError::from)
    }
}

async fn run(a: &Value) -> Result<String, String> {
    let q = a["query"].as_str().ok_or("query is required")?;
    let limit = a["limit"].as_u64().unwrap_or(8).clamp(1, 30).to_string();
    let mut cmd = tokio::process::Command::new("chatstrata");
    cmd.args(["search", "--json", "--limit", &limit]);
    if let Some(since) = a["since"].as_str() {
        cmd.args(["--since", since]);
    }
    cmd.arg(q);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("chatstrata isn't available ({e}); install it to search past sessions"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    let hits: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    if hits.is_empty() {
        return Ok("no past sessions matched".into());
    }
    Ok(hits
        .iter()
        .map(|h| {
            format!(
                "[{} · {} · {}] {}\n  {}",
                h["created_at"].as_str().unwrap_or("").get(..16).unwrap_or(""),
                h["source"].as_str().unwrap_or(""),
                h["project"].as_str().unwrap_or(""),
                crate::llm::truncate(h["title"].as_str().unwrap_or(""), 80),
                h["snippet"].as_str().unwrap_or("").replace('\n', " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
