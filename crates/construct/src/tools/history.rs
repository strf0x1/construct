//! History tools over this project's journal: `recall_result` (an earlier
//! tool call's output by call id) and `journal_query` (read-only SQL).
//!
//! The design choice a reader must know: both open the journal in-process,
//! read-only, and only this project's file (`journal::path`). The sandbox
//! still keeps `journal.sqlite` out of shell and file tools; these two are
//! the only way in, and every limit on the SQL one (docs/adr/0009) is
//! enforced by SQLite itself, not by parsing the query.

use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, def, obj};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A query running longer than this is interrupted.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Rows and bytes of result text returned; past either, the result is cut
/// with a note.
const MAX_ROWS: usize = 200;
const MAX_BYTES: usize = 16 * 1024;
/// Pragmas that only describe; their argument is at most a table name.
const READ_PRAGMAS: &[&str] = &["data_version", "table_info", "table_xinfo", "index_list", "index_info"];

pub struct RecallResult;

impl RecallResult {
    pub const NAME: &str = "recall_result";
}

#[async_trait::async_trait]
impl Tool for RecallResult {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn group(&self) -> Group {
        Group::History
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Return the full original output of an earlier tool call in this project, by its call id. Use when an earlier result was cut, summarized or is no longer in view.",
            obj(json!({ "id": { "type": "string", "description": "The tool call id" } }), &["id"]),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "id", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        let id = a["id"].as_str().ok_or("id is required")?.to_string();
        let file = crate::journal::path(&ctx.paths, &ctx.cwd);
        let found = blocking(move || {
            let conn = open(file)?;
            // The expression matches the `events_id` index exactly. Ids can
            // repeat across sessions (some providers number them); newest wins.
            conn.query_row(
                "SELECT json_extract(payload, '$.message.content') FROM events
                 WHERE json_extract(payload, '$.id') = ?1 AND type = 'tool_result'
                 ORDER BY seq DESC LIMIT 1",
                [&id],
                |r| r.get::<_, Option<String>>(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ToolError::new("not_found", format!("no tool result with id {id:?} in this project's journal"))
                }
                e => ToolError::new("error", e.to_string()),
            })
        })
        .await?;
        Ok(Output::plain(found.unwrap_or_default()))
    }
}

pub struct JournalQuery;

impl JournalQuery {
    pub const NAME: &str = "journal_query";
}

#[async_trait::async_trait]
impl Tool for JournalQuery {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }
    fn group(&self) -> Group {
        Group::History
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Read-only SQLite over this project's journal: every past session's messages and tool calls. One SELECT (or WITH) statement; at most 200 rows / 16 KB come back, and a query is stopped after 5 s.\n\
             Table events(seq INTEGER PRIMARY KEY, session_id TEXT, type TEXT, v INT, at_ms INT unix ms, payload TEXT JSON).\n\
             type and its payload fields: head (cwd, model; a session's first row), user / assistant (message.content; assistant also message.tool_calls), tool_start (id, name, args), tool_end (id, name, ok, ms, code, output), tool_result (id, message.content), route (groups, skills), request, usage, turn_start, turn_end, compacted, error.\n\
             Read a field with json_extract(payload, '$.name'), e.g. WHERE type = 'tool_end' AND json_extract(payload, '$.ok') = 0.\n\
             Keyword search over user, assistant and tool_result text: SELECT e.session_id, e.type, snippet(events_fts, 0, '[', ']', '…', 12) FROM events_fts JOIN events e ON e.seq = events_fts.rowid WHERE events_fts MATCH 'word' ORDER BY rank LIMIT 20.",
            obj(json!({ "sql": { "type": "string" } }), &["sql"]),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "sql", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        let sql = a["sql"].as_str().ok_or("sql is required")?.to_string();
        let file = crate::journal::path(&ctx.paths, &ctx.cwd);
        let cancel = ctx.cancel.clone();
        blocking(move || {
            let conn = open(file)?;
            query(&conn, &sql, cancel)
        })
        .await
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ToolError::from(e.to_string()))?
}

/// This project's journal, read-only three ways: the open flags, `PRAGMA
/// query_only`, and an authorizer that allows reading and nothing else
/// (no ATTACH, no writes, no transactions, and no PRAGMA beyond the few that
/// only describe: FTS5 reads `data_version` on every MATCH).
fn open(file: PathBuf) -> Result<Connection, ToolError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&file, flags)
        .map_err(|e| ToolError::new("not_found", format!("this project has no journal yet ({e})")))?;
    let fail = |e: rusqlite::Error| ToolError::new("error", e.to_string());
    conn.pragma_update(None, "query_only", true).map_err(fail)?;
    conn.busy_timeout(Duration::from_secs(2)).map_err(fail)?;
    conn.authorizer(Some(|c: AuthContext<'_>| match c.action {
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Function { .. } | AuthAction::Recursive => {
            Authorization::Allow
        }
        AuthAction::Pragma { pragma_name, .. } if READ_PRAGMAS.contains(&pragma_name) => Authorization::Allow,
        _ => Authorization::Deny,
    }))
    .map_err(fail)?;
    Ok(conn)
}

fn query(conn: &Connection, sql: &str, cancel: tokio::sync::watch::Receiver<bool>) -> Result<Output, ToolError> {
    let word = first_word(sql).to_ascii_lowercase();
    if word != "select" && word != "with" {
        return Err(refused(&format!("{} is not a SELECT", if word.is_empty() { "an empty query" } else { &word })));
    }
    let mut st = conn.prepare(sql).map_err(sql_error)?;
    // `WITH ... DELETE` passes the keyword check; the statement itself knows.
    if !st.readonly() {
        return Err(refused("this statement writes"));
    }
    let deadline = Instant::now() + TIMEOUT;
    conn.progress_handler(1000, Some(move || Instant::now() > deadline || *cancel.borrow()))
        .map_err(|e| ToolError::from(e.to_string()))?;

    let cols: Vec<String> = st.column_names().into_iter().map(String::from).collect();
    let mut text = cols.join(" | ");
    let (mut n, mut cut) = (0, None);
    let mut rows = st.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        if n == MAX_ROWS {
            cut = Some(format!("{MAX_ROWS} rows"));
            break;
        }
        let line = (0..cols.len()).map(|i| cell(row.get_ref(i))).collect::<Vec<_>>().join(" | ");
        if text.len() + 1 + line.len() > MAX_BYTES {
            let room = MAX_BYTES.saturating_sub(text.len() + 1);
            text.push('\n');
            text.push_str(&crate::llm::truncate(&line, room));
            cut = Some(format!("{} KB", MAX_BYTES / 1024));
            break;
        }
        text.push('\n');
        text.push_str(&line);
        n += 1;
    }
    if n == 0 && cut.is_none() {
        text.push_str("\n(no rows)");
    }
    if let Some(at) = &cut {
        text.push_str(&format!(
            "\n[result cut at {at}: add a LIMIT, filter on type or session_id, or select substr(...) of long fields]"
        ));
    }
    Ok(Output::plain(text).with_data(json!({ "rows": n, "truncated": cut.is_some() })))
}

fn refused(why: &str) -> ToolError {
    ToolError::new("refused", format!("{why}; journal_query runs one read-only SELECT"))
}

fn sql_error(e: rusqlite::Error) -> ToolError {
    if matches!(e, rusqlite::Error::MultipleStatement) {
        return refused("more than one statement");
    }
    // The authorizer's denial surfaces as SQLITE_AUTH, or as a plain error
    // saying so when a table-valued pragma is refused.
    if e.sqlite_error_code() == Some(rusqlite::ErrorCode::AuthorizationForStatementDenied)
        || e.to_string().contains("not authorized")
    {
        return refused("not allowed");
    }
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::OperationInterrupted) => ToolError::new(
            "timeout",
            format!(
                "query stopped after {} s; narrow it: filter on type or session_id, use events_fts MATCH, add a LIMIT",
                TIMEOUT.as_secs()
            ),
        ),
        _ => ToolError::new("sql", e.to_string()),
    }
}

/// One value on one line: text with its newlines escaped, blobs by size.
fn cell(v: rusqlite::Result<ValueRef<'_>>) -> String {
    match v {
        Ok(ValueRef::Null) | Err(_) => "NULL".into(),
        Ok(ValueRef::Integer(i)) => i.to_string(),
        Ok(ValueRef::Real(f)) => f.to_string(),
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).replace('\\', "\\\\").replace('\n', "\\n"),
        Ok(ValueRef::Blob(b)) => format!("<blob {} bytes>", b.len()),
    }
}

/// The statement's first keyword, past whitespace and comments.
fn first_word(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else {
            break;
        }
    }
    let end = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::first_word;

    #[test]
    fn first_word_skips_comments() {
        assert_eq!(first_word("  -- hi\n /* x */ SELECT 1"), "SELECT");
        assert_eq!(first_word("with x as (select 1) select * from x"), "with");
        assert_eq!(first_word("ATTACH 'x' AS y"), "ATTACH");
        assert_eq!(first_word("/* never closed"), "");
    }
}
