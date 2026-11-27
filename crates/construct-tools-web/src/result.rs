//! Shared search result types and text helpers used by every `web_search`
//! backend.
//!
//! The design choice: keeping `Hit`, `Answer`, HTML tag stripping, and key
//! hint formatting in a leaf module lets each backend (including the keyless
//! DuckDuckGo fallback) stay independent of the backend planner in `search`.

use construct_core::secrets::{BRAVE_API_KEY, EXA_API_KEY, TAVILY_API_KEY};

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

/// Which keys to suggest when the keyless fallback is rate limited.
pub(super) fn key_names() -> String {
    format!("{EXA_API_KEY}, {BRAVE_API_KEY} or {TAVILY_API_KEY}")
}

/// Strip HTML tags and collapse runs of whitespace into single spaces.
pub(super) fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
