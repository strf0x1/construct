//! hashline: read shows every line as `LINE:HASH|content`, and edit takes a
//! batch of line operations addressed by those anchors. Each anchor is
//! checked against the file as it is on disk; any mismatch rejects the whole
//! batch with fresh anchors around the failure, and nothing is ever relocated.
//! Ported from omp-hashline-strict v0.1; the descriptions are the whole
//! format teaching, so keep their examples literal.

pub mod anchors;
pub mod apply;
pub mod format;
pub mod fsio;
pub mod hash;
pub mod ops;

use super::super::fs::{encoding_error, resolve};
use super::super::read::{LINE_MAX, READ_LINES, list_dir};
use super::super::{Ctx, Display, Group, Output, Tool, ToolError, arg, blocking, def, num, obj};
use crate::config::{AnchorMode, HashlineConfig};
use fsio::ReadError;
use serde_json::{Value, json};

const READ_DESCRIPTION: &str = "Read a text file (or list a directory). Every line is returned as `LINE:HASH|content`, e.g. `12:ax|return x;`. `LINE:HASH` is that line's anchor: copy it exactly into `edit` operations. Anchors change whenever the file changes, so always take them from the LATEST read or edit output for the file. Reads up to 2000 lines; use offset/limit for more.";

const READ_DESCRIPTION_HASH: &str = "Read a text file (or list a directory). Every line is returned as `HASH|content`, e.g. `ax|return x;`. `HASH` is that line's anchor: copy it exactly into `edit` operations. Anchors change whenever the file changes, so always take them from the LATEST read or edit output for the file. Reads up to 2000 lines; use offset/limit for more.";

const EDIT_DESCRIPTION: &str = r#"Edit a text file using hash-anchored line operations.

Every line in `read` output looks like `LINE:HASH|content`, e.g. `12:ax|return x;`. The `LINE:HASH` pair before the `|` is that line's anchor. Anchors prove you are editing the line you think you are: if the file changed since you read it, the batch is REJECTED with fresh anchors — nothing is ever silently applied to the wrong line.

Operations (entries in `edits`):
- {"op":"set","anchor":"12:ax","text":"new content"} — replace one line
- {"op":"replace_range","start":"12:ax","end":"15:qy","text":"..."} — replace lines 12-15 (inclusive)
- {"op":"insert_after","anchor":"12:ax","text":"..."} — insert new line(s) after line 12
- {"op":"insert_before","anchor":"12:ax","text":"..."} — insert new line(s) before line 12
- {"op":"delete","anchor":"12:ax"} — delete one line
- {"op":"delete_range","start":"12:ax","end":"15:qy"} — delete lines 12-15 (inclusive)

Rules:
- Copy anchors EXACTLY from the most recent read or edit output for this file. Never guess a hash, reuse a stale one, or fabricate one.
- `text` may hold multiple lines (separate with \n) and becomes file content verbatim — do NOT include anchors, hashes, or line numbers in it. For a row shown as `12:ax|old`, the replacement text is `"new"` — never `"ax|new"` or `"12:ax|new"`.
- The batch is atomic: if ANY edit fails validation, NOTHING is applied, and the error shows the current content around each failure so you can fix and resend in one turn.
- A successful edit returns fresh anchors for the edited region. Use those anchors for follow-up edits — do NOT re-read the file just to get new anchors.
- Edits in one batch must not overlap: each line may be touched by at most one op.
- To create a new file, or to fully rewrite one, use `write` instead.

Example call:
{"path":"src/app.ts","edits":[
  {"op":"set","anchor":"3:kf","text":"import { z } from \"zod\";"},
  {"op":"insert_after","anchor":"10:vm","text":"const t0 = performance.now();"}
]}"#;

const EDIT_DESCRIPTION_HASH: &str = r#"Edit a text file using hash-anchored line operations.

Every line in `read` output looks like `HASH|content`, e.g. `ax|return x;`. The `HASH` before the `|` is that line's anchor: it names that line's content. If the content you targeted was changed or removed since you read it, the batch is REJECTED — nothing is applied.

Operations (entries in `edits`):
- {"op":"set","anchor":"ax","text":"new content"} — replace one line
- {"op":"replace_range","start":"ax","end":"qy","text":"..."} — replace the lines from ax to qy (inclusive)
- {"op":"insert_after","anchor":"ax","text":"..."} — insert new line(s) after line ax
- {"op":"insert_before","anchor":"ax","text":"..."} — insert new line(s) before line ax
- {"op":"delete","anchor":"ax"} — delete one line
- {"op":"delete_range","start":"ax","end":"qy"} — delete the lines from ax to qy (inclusive)

Rules:
- Copy anchors EXACTLY from the most recent read or edit output for this file. Never guess a hash, reuse a stale one, or fabricate one.
- `text` may hold multiple lines (separate with \n) and becomes file content verbatim — do NOT include anchors or hashes in it. For a row shown as `ax|old`, the replacement text is `"new"` — never `"ax|new"`.
- The batch is atomic: if ANY edit fails validation, NOTHING is applied, and the error says which edits failed so you can fix and resend in one turn.
- A successful edit returns fresh anchors for the edited region. Use those anchors for follow-up edits — do NOT re-read the file just to get new anchors.
- Edits in one batch must not overlap: each line may be touched by at most one op.
- To create a new file, or to fully rewrite one, use `write` instead.

Example call:
{"path":"src/app.ts","edits":[
  {"op":"set","anchor":"kf","text":"import { z } from \"zod\";"},
  {"op":"insert_after","anchor":"vm","text":"const t0 = performance.now();"}
]}"#;

pub struct HashlineRead {
    min_hash_len: usize,
    mode: AnchorMode,
}

impl HashlineRead {
    pub const NAME: &str = "read";
    pub fn new(cfg: &HashlineConfig) -> Self {
        Self { min_hash_len: cfg.min_hash_len, mode: cfg.anchor }
    }
}

#[async_trait::async_trait]
impl Tool for HashlineRead {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            match self.mode {
                AnchorMode::LineHash => READ_DESCRIPTION,
                AnchorMode::Hash => READ_DESCRIPTION_HASH,
            },
            obj(
                json!({
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "description": "1-based first line" },
                    "limit": { "type": "integer" }
                }),
                &["path"],
            ),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "path", preview_lines: 0 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        blocking(ctx, || self.run(ctx, &a))
    }
}

impl HashlineRead {
    fn run(&self, ctx: &Ctx, a: &Value) -> Result<String, ToolError> {
        let path = resolve(ctx, arg(a, "path").ok_or("path is required")?);
        ctx.sandbox.check_read(&path).map_err(ToolError::sandbox)?;
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            return list_dir(&path);
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = bytes.len();
        let (text, _) = match fsio::decode(bytes) {
            Ok(t) => t,
            Err(ReadError::Binary) => {
                return Err(format!("{} looks binary ({len} bytes); not shown", path.display()).into());
            }
            Err(ReadError::Utf16) => return Err(encoding_error(&path.display().to_string(), true)),
            Err(ReadError::NotUtf8(bytes)) => return Ok(Self::unanchored(&path, &bytes, a)),
            Err(ReadError::Io(e)) => return Err(format!("{}: {e}", path.display()).into()),
        };
        let (lines, _) = fsio::split(&text);
        let map = anchors::build(lines, self.min_hash_len, self.mode);
        let total = map.len();
        let offset = num(a, "offset").unwrap_or(1).max(1);
        let limit = num(a, "limit").unwrap_or(READ_LINES);
        let mut out = String::with_capacity(len.min(256 * 1024));
        let mut truncated = vec![];
        for n in (offset..=total).take(limit) {
            let line = &map.lines[n - 1];
            if line.len() > LINE_MAX {
                // Shown cut short, so it gets no anchor: a `set` built from it
                // would drop the rest of the line.
                out.push_str(&format!("{n}|{}\n", crate::llm::truncate(line, LINE_MAX)));
                truncated.push(n.to_string());
            } else {
                out.push_str(&map.row(n));
                out.push('\n');
            }
        }
        if !truncated.is_empty() {
            out.push_str(&format!(
                "(line {} is over {LINE_MAX} chars and shown cut short, so it has no anchor)\n",
                truncated.join(", ")
            ));
        }
        let shown_end = (offset - 1 + limit).min(total);
        if total == 0 {
            out.push_str("(empty file)");
        } else if shown_end < total {
            out.push_str(&format!("… {total} lines total; showing {offset}-{shown_end}. Use offset to read more.\n"));
        }
        ctx.mark_read(&path);
        Ok(out)
    }

    /// A file that isn't UTF-8, shown so the model can see it but without
    /// anchors, since edit will refuse it. Not marked read either: a `write`
    /// of what is shown here would turn every bad byte into U+FFFD.
    fn unanchored(path: &std::path::Path, bytes: &[u8], a: &Value) -> String {
        let text = String::from_utf8_lossy(bytes);
        let (lines, _) = fsio::split(&text);
        let offset = num(a, "offset").unwrap_or(1).max(1);
        let limit = num(a, "limit").unwrap_or(READ_LINES);
        let note = encoding_error(&path.display().to_string(), false).message;
        let mut out = format!("({note} Shown with \u{fffd} for the bad bytes and without anchors.)\n");
        for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
            out.push_str(&format!("{}|{}\n", i + 1, crate::llm::truncate(line, LINE_MAX)));
        }
        let shown_end = (offset - 1 + limit).min(lines.len());
        if shown_end < lines.len() {
            out.push_str(&format!(
                "… {} lines total; showing {offset}-{shown_end}. Use offset to read more.\n",
                lines.len()
            ));
        }
        out
    }
}

pub struct HashlineEdit {
    cfg: HashlineConfig,
}

impl HashlineEdit {
    pub const NAME: &str = "edit";
    pub fn new(cfg: &HashlineConfig) -> Self {
        Self { cfg: cfg.clone() }
    }
}

#[async_trait::async_trait]
impl Tool for HashlineEdit {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        let (description, form, example) = match self.cfg.anchor {
            AnchorMode::LineHash => (EDIT_DESCRIPTION, "LINE:HASH", "12:ax"),
            AnchorMode::Hash => (EDIT_DESCRIPTION_HASH, "HASH", "ax"),
        };
        let anchor = |d: &str| json!({ "type": "string", "description": d.replace("LINE:HASH", form).replace("12:ax", example) });
        def(
            Self::NAME,
            description,
            obj(
                json!({
                    "path": { "type": "string", "description": "Path of the file to edit (absolute, or relative to the working directory)." },
                    "edits": {
                        "type": "array",
                        "description": "Operations to apply as one atomic batch.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": ops::OP_NAMES, "description": "Operation kind." },
                                "anchor": anchor("Anchor \"LINE:HASH\" for set/insert_after/insert_before/delete, copied from read output."),
                                "start": anchor("Range start anchor \"LINE:HASH\" for replace_range/delete_range."),
                                "end": anchor("Range end anchor \"LINE:HASH\" (inclusive) for replace_range/delete_range."),
                                "text": { "type": "string", "description": "Replacement/inserted content, verbatim; \\n-separated for multiple lines. Required for set/replace_range/insert_*." }
                            },
                            "required": ["op"]
                        }
                    }
                }),
                &["path", "edits"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "path", preview_lines: 8 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        self.run(ctx, &a)
    }
}

impl HashlineEdit {
    fn run(&self, ctx: &Ctx, a: &Value) -> Result<Output, ToolError> {
        let raw = arg(a, "path").unwrap_or("");
        let edits = a.get("edits").and_then(Value::as_array);
        let fail = |code: &'static str, message: String| {
            ToolError::new(code, message.clone()).with_data(
                json!({ "status": "error", "path": raw, "op_count": edits.map_or(0, Vec::len), "message": message }),
            )
        };
        // Models trained on other edit tools send their shapes; point them at this one.
        if edits.is_none() && (a.get("old_string").is_some() || a.get("input").is_some()) {
            return Err(fail(
                "legacy_input",
                r#"This edit tool takes anchored line operations, not old_string/new_string or a patch string. Send {"path": "...", "edits": [{"op": "set", "anchor": "LINE:HASH", "text": "..."}, ...]} using anchors from the latest read output."#.into(),
            ));
        }
        if raw.is_empty() {
            return Err(fail("MISSING_FIELD", "\"path\" is required.".into()));
        }
        let Some(edits) = edits.filter(|e| !e.is_empty()) else {
            return Err(fail("MISSING_FIELD", "\"edits\" must be a non-empty array of operations.".into()));
        };
        let path = resolve(ctx, raw);
        ctx.sandbox.check_write(&path).map_err(ToolError::sandbox)?;
        if !path.exists() {
            return Err(fail("not_found", format!("{raw} does not exist. Use the write tool to create new files.")));
        }
        if !path.is_file() {
            return Err(fail("not_a_file", format!("{raw} is not a regular file.")));
        }
        let snap = match fsio::read(&path) {
            Ok(s) => s,
            Err(ReadError::Binary) => {
                return Err(fail(
                    "binary",
                    format!("{raw} looks binary (contains NUL bytes); hashline edits work on text files only"),
                ));
            }
            Err(e @ (ReadError::Utf16 | ReadError::NotUtf8(_))) => {
                let e = encoding_error(raw, matches!(e, ReadError::Utf16));
                return Err(fail(e.code, e.message));
            }
            Err(ReadError::Io(e)) => return Err(fail("io", format!("{raw}: {e}"))),
        };
        if snap.lines.is_empty() {
            return Err(fail(
                "empty_file",
                format!("{raw} is empty; there are no lines to anchor to. Use the write tool to set its content."),
            ));
        }

        let cfg = &self.cfg;
        let map = anchors::build(snap.lines.clone(), cfg.min_hash_len, cfg.anchor);
        let (resolved, errors) = ops::validate(&map, edits, cfg.max_ops, cfg.strip_echoes);

        // Forgiveness is never silent: it is in the text and in the data, so
        // a bench can count every one.
        let stripped: Vec<usize> = resolved.iter().filter(|r| r.echo_stripped).map(|r| r.index).collect();
        let note = if stripped.is_empty() {
            String::new()
        } else {
            let idx: Vec<String> = stripped.iter().map(ToString::to_string).collect();
            format!(
                "\nnote: removed echoed anchor prefix(es) from the text of edits[{}] — \"text\" must be file content only; anchors never belong inside it.",
                idx.join(", ")
            )
        };
        let mut data = json!({ "path": raw, "op_count": edits.len() });
        if !stripped.is_empty() {
            data["echo_stripped"] = json!(stripped);
        }

        if !errors.is_empty() {
            let text = format::rejection(raw, edits.len(), &errors, &map, cfg.window_radius);
            data["status"] = json!("rejected");
            data["errors"] = json!(errors);
            return Err(ToolError::new(errors[0].code, text + &note).with_data(data));
        }

        let outcome = apply::apply(&snap.lines, &resolved);
        fsio::write(&snap, &outcome.lines).map_err(|e| fail("io", format!("{raw}: {e}")))?;
        ctx.mark_read(&path);
        let new = anchors::build(outcome.lines, cfg.min_hash_len, cfg.anchor);
        let text =
            format::success(raw, &outcome.regions, &new, snap.lines.len(), outcome.line_delta, cfg.context_lines);
        data["status"] = json!("applied");
        data["regions"] = json!(outcome.regions);
        data["line_delta"] = json!(outcome.line_delta);
        Ok(Output::plain(text + &note).with_data(data))
    }
}
