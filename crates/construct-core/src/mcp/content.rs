//! A `tools/call` result as text for the model.
//!
//! The design choice a reader must know: nothing the server returned as text
//! is dropped. Text blocks and embedded resources' text pass through, links
//! and binary blocks become one line naming what they were, and
//! `structuredContent` stands in when there is no content to show.

use serde_json::Value;

pub fn render(res: &Value) -> String {
    let blocks: Vec<String> = res["content"].as_array().map(|c| c.iter().map(block).collect()).unwrap_or_default();
    if blocks.iter().all(|b| b.trim().is_empty())
        && let Some(s) = res.get("structuredContent")
    {
        return s.to_string();
    }
    if res.get("content").is_none() {
        return res.to_string();
    }
    blocks.join("\n")
}

fn block(b: &Value) -> String {
    let s = |k: &str| b[k].as_str().unwrap_or_default();
    match b["type"].as_str() {
        Some("text") => s("text").to_string(),
        Some("resource") => {
            let r = &b["resource"];
            match r["text"].as_str() {
                Some(t) => t.to_string(),
                None => format!("[resource {} ({})]", r["uri"].as_str().unwrap_or("?"), mime(r)),
            }
        }
        Some("resource_link") => {
            let name = if s("name").is_empty() { s("uri") } else { s("name") };
            let desc = if s("description").is_empty() { String::new() } else { format!(": {}", s("description")) };
            format!("[link {name} {}{desc}]", s("uri"))
        }
        Some(t @ ("image" | "audio")) => format!("[{t} ({})]", mime(b)),
        Some(t) => format!("[{t} content]"),
        None => b.to_string(),
    }
}

fn mime(v: &Value) -> &str {
    v["mimeType"].as_str().unwrap_or("unknown type")
}

#[cfg(test)]
mod tests {
    use super::render;
    use serde_json::json;

    #[test]
    fn keeps_resource_text_links_and_structured_content() {
        let r = json!({ "content": [
            { "type": "text", "text": "one" },
            { "type": "resource", "resource": { "uri": "file:///a.md", "text": "embedded" } },
            { "type": "resource", "resource": { "uri": "file:///b.png", "blob": "AA==", "mimeType": "image/png" } },
            { "type": "resource_link", "uri": "file:///c.rs", "name": "c.rs", "description": "the parser" },
            { "type": "image", "data": "AA==", "mimeType": "image/png" }
        ] });
        assert_eq!(
            render(&r),
            "one\nembedded\n[resource file:///b.png (image/png)]\n[link c.rs file:///c.rs: the parser]\n[image (image/png)]"
        );
        assert_eq!(render(&json!({ "content": [], "structuredContent": { "n": 3 } })), r#"{"n":3}"#);
    }
}
