//! What a SKILL.md looks like: the name rule, the frontmatter split and the
//! normalising a write goes through. Pure text in, text or a reason out; the
//! library decides where files live.

use std::collections::BTreeMap;

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Split `---\nkey: value\n---\nbody` frontmatter. Values may be quoted.
pub fn frontmatter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    let Some(rest) = text.strip_prefix("---") else { return (map, text) };
    let Some((head, tail)) = rest.split_once("\n---") else { return (map, text) };
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.starts_with(' ') && !v.is_empty() {
                map.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    let body = tail.trim_start_matches(['\r', '\n']);
    (map, body)
}

/// Ensure SKILL.md has frontmatter with the right name and a description.
pub(super) fn normalize(name: &str, content: &str) -> Result<String, String> {
    let (fm, body) = frontmatter(content);
    let description = fm.get("description").cloned().unwrap_or_default();
    if description.trim().is_empty() {
        return Err(
            "SKILL.md needs frontmatter with a description: ---\\nname: x\\ndescription: when to use it\\n---".into()
        );
    }
    if description.len() > 300 {
        return Err("keep the description under 300 chars; put detail in the body".into());
    }
    if body.trim().is_empty() {
        return Err("SKILL.md body is empty".into());
    }
    // A loaded skill enters the prompt like memory does.
    if let Some(why) = crate::guard::check(content) {
        return Err(why);
    }
    Ok(format!("---\nname: {name}\ndescription: {description}\n---\n\n{}\n", body.trim_end()))
}
