//! The keyless fallback: DuckDuckGo's HTML endpoint, scraped. It needs no
//! account, so `web_search` works on a fresh install, but the endpoint is
//! undocumented and rate-limits; every keyed backend is preferred to it.

use super::search::Hit;
use crate::tools::Ctx;
use std::time::Duration;

pub(super) async fn search(ctx: &Ctx, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let html = ctx
        .http
        .post("https://html.duckduckgo.com/html/")
        .header("user-agent", "Mozilla/5.0 (X11; Linux) construct")
        .form(&[("q", q)])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
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
        out.push((decode_entities(&title), url, decode_entities(&snippet)));
        if out.len() >= n {
            break;
        }
    }
    if out.is_empty() && html.contains("anomaly") {
        return Err(format!(
            "DuckDuckGo rate-limited this request; set {} with `construct secrets set`",
            super::search::key_names()
        ));
    }
    Ok(out)
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
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
            && i + 2 < b.len()
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
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
