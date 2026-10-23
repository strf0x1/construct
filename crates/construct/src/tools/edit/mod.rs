//! edit: exact-text replacement through `replace::replace_once`. The
//! replacement itself is a plain function, also used to patch skills, and is
//! never swapped by `[tools] editor`. `hashline` is the other editor family.

pub mod hashline;
pub mod replace;

use super::fs::{encoding_error, is_utf16, resolve};
use super::{Ctx, Display, Group, Output, OutputKind, Tool, ToolError, ToolKind, arg, def, obj};
use serde_json::{Value, json};

pub struct Edit;

impl Edit {
    pub const NAME: &str = "edit";
}

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Replace exact text in a file. old_string must match once (copy it from read output without the line-number prefix) unless replace_all is set. Tolerates quote/dash and trailing-whitespace differences.",
            obj(
                json!({
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                }),
                &["path", "old_string", "new_string"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "path", preview_lines: 8 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        ctx.can_write(Self::NAME, &a, "path").await?;
        run(ctx, &a).map(|s| Output::plain(s).with_kind(OutputKind::Diff))
    }
}

fn run(ctx: &Ctx, a: &Value) -> Result<String, ToolError> {
    let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
    let old = arg(a, "old_string").ok_or("old_string is required")?;
    let new = arg(a, "new_string").ok_or("new_string is required")?;
    let all = a.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let shown = path.display().to_string();
    if is_utf16(&bytes) {
        return Err(encoding_error(&shown, true));
    }
    let text = String::from_utf8(bytes).map_err(|_| encoding_error(&shown, false))?;
    let updated = replace::replace_once(&text, old, new, all)?;
    std::fs::write(&path, &updated).map_err(|e| e.to_string())?;
    ctx.mark_read(&path);
    Ok(format!("edited {}\n{}", path.display(), diff_summary(&text, &updated)))
}

/// A compact unified diff so the model (and the UI) can see what changed.
pub fn diff_summary(before: &str, after: &str) -> String {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut out = String::new();
    for group in diff.grouped_ops(1).iter().take(6) {
        for op in group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                };
                let line = change.value().trim_end_matches('\n');
                out.push_str(&format!("{sign}{}\n", crate::llm::truncate(line, 240)));
            }
        }
        out.push_str("…\n");
    }
    crate::llm::truncate(&out, 4000)
}
