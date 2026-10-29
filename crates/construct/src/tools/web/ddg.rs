//! The keyless fallback: DuckDuckGo's HTML endpoint, scraped. It needs no
//! account, so `web_search` works on a fresh install, but the endpoint is
//! undocumented and rate-limits; every keyed backend is preferred to it.

use super::{
    filter::Filter,
    search::{Answer, Hit},
};
use crate::tools::Ctx;
use std::time::Duration;

pub(super) async fn search(ctx: &Ctx, q: &str, n: usize, f: &Filter) -> Result<Answer, String> {
    let (q, site_note) = f.site_query(q);
    let mut notes: Vec<String> = site_note.into_iter().collect();
    if f.recency.is_some() {
        notes.push("recency is not supported by DuckDuckGo and was ignored".into());
    }
    let html = ctx
        .http
        .post("https://html.duckduckgo.com/html/")
        .header("user-agent", "Mozilla/5.0 (X11; Linux) construct")
        .form(&[("q", q.as_str())])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let out = parse(&html, n);
    if out.is_empty() && html.contains("anomaly") {
        return Err(format!(
            "DuckDuckGo rate-limited this request; set {} with `construct secrets set`",
            super::search::key_names()
        ));
    }
    Ok(Answer { backend: "DuckDuckGo", hits: out, notes })
}

/// The results on a DuckDuckGo HTML page, ads left out, at most `n`.
fn parse(html: &str, n: usize) -> Vec<Hit> {
    let mut out = Vec::new();
    for block in html.split("class=\"result__a\"").skip(1) {
        let href = between(block, "href=\"", "\"").unwrap_or_default();
        let title = between(block, ">", "</a>").map(strip_tags).unwrap_or_default();
        let snippet = block
            .split("class=\"result__snippet\"")
            .nth(1)
            .and_then(|s| between(s, ">", "</a>"))
            .map(strip_tags)
            .unwrap_or_default();
        let url = match href.split("uddg=").nth(1) {
            Some(enc) => percent_decode(enc.split('&').next().unwrap_or(enc)),
            None => href.to_string(),
        };
        if url.contains("duckduckgo.com/y.js") || title.is_empty() {
            continue; // ads
        }
        out.push(Hit { title: decode_entities(&title), url, snippet: decode_entities(&snippet), date: None });
        if out.len() >= n {
            break;
        }
    }
    out
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    Some(s.split_once(a)?.1.split_once(b)?.0)
}

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

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = b.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use crate::text::prop;
    use proptest::prelude::*;

    const PAGE: &[&str] = &[
        "class=\"result__a\"", "class=\"result__snippet\"", "href=\"", "\"", ">", "</a>", "<b>", "uddg=", "%2F", "%E2%82",
        "%", "+", "&amp;", "&", "duckduckgo.com/y.js",
    ];

    proptest! {
        #![proptest_config(prop::config())]

        #[test]
        fn any_page_parses_without_panicking(html in prop::text_with(PAGE), n in 0usize..8) {
            prop_assert!(super::parse(&html, n).len() <= n.max(1));
        }

        /// Text with no escape in it comes back as it was.
        #[test]
        fn percent_decode_leaves_plain_text_alone(s in prop::text()) {
            let s = s.replace(['%', '+'], "");
            prop_assert_eq!(super::percent_decode(&s), s);
        }
    }

    #[test]
    fn percent_decode_leaves_a_broken_escape_before_multibyte_text() {
        assert_eq!(super::percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(super::percent_decode("%aé"), "%aé");
        assert_eq!(super::percent_decode("50%"), "50%");
    }
}
