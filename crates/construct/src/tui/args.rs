//! Showing a tool call: the one argument its display hints name.

/// The argument a tool's display hints name, or the raw arguments.
pub(super) fn primary_arg(key: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let s = match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => args.to_string(),
    };
    let s = s.replace('\n', " ⏎ ");
    crate::llm::truncate(&s, 200)
}
