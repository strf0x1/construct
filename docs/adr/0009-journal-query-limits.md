# 0009 — journal_query: read-only SQL, enforced by SQLite

Status: accepted, 2026-10-21. Implements #31; the decision to ship SQL from
day one is #29's (6), not re-argued here.

## Context

#29 gave the model two ways into the journal (ADR 0008): `recall_result`, a
call id to the output the model saw, and `journal_query`, SQL over this
project's `events` table. They replace `session_search`, which shelled out to
chatstrata and failed without it on `PATH`.

A SQL tool hands the model a query language over a file the sandbox otherwise
keeps from every tool. What it must not be able to do: write or delete rows,
change the file's mode (`PRAGMA journal_mode=DELETE`), reach another database
(`ATTACH`), run long enough to stall a turn, or return a result big enough to
crowd out the context. Parsing SQL to check this is the brittle option: the
grammar has `WITH ... DELETE`, comments, table-valued pragmas, and several
statements in one string.

## Decision

`journal_query` opens `journal::path(paths, cwd)`, this project's file and no
other, in-process, on a fresh connection per call, and:

- **Read-only three ways, all SQLite's own.** `SQLITE_OPEN_READ_ONLY`, then
  `PRAGMA query_only = 1`, then an authorizer that allows `SELECT`, column
  reads, functions and recursive CTEs, plus the describe-only pragmas
  `data_version` (FTS5 reads it on every `MATCH`), `table_info`,
  `table_xinfo`, `index_list` and `index_info`. Everything else is denied
  while the statement is prepared: `ATTACH`, `DETACH`, every write, DDL,
  transactions and savepoints, and every other pragma, including the
  table-valued form.
- **One SELECT.** The first keyword (past whitespace and comments) must be
  `SELECT` or `WITH`; `prepare` must leave no second statement; and
  `sqlite3_stmt_readonly` must hold, which catches `WITH ... DELETE`. Each
  refusal says "journal_query runs one read-only SELECT" and carries the code
  `refused`. The keyword check is the friendly message; the authorizer and the
  flags are the guarantee.
- **5 s timeout** by a progress handler every 1000 VM steps, which also stops
  the query when the turn is interrupted. The error (`timeout`) tells the
  model how to narrow the query.
- **200 rows and 16 KB** of result text, a header line then one line per row
  (`a | b`, newlines escaped, blobs as their size). Past either cap the result
  stops there and ends with a note naming the cap and how to narrow; `data`
  carries `rows` and `truncated`.
- **The schema is in the description**: the table, its columns, the event
  types and their payload fields, the `json_extract` idiom, and a `MATCH`
  query on `events_fts`, so a small model can write a query without exploring.

`recall_result` uses the same read-only open and one indexed lookup
(`json_extract(payload, '$.id')`, `type = 'tool_result'`), newest first,
since some providers number call ids per session.

Both are in `Group::History`, routed rather than core. The sandbox does not
change: shell and file tools still cannot read `journal.sqlite`.

## Consequences

- Nothing in construct needs chatstrata. It stays a consumer, through
  `construct sessions export` or by attaching the file itself.
- The limits are constants in `tools/history.rs`, not config. A query that
  legitimately needs longer than 5 s over one project's journal is a sign the
  journal needs an index, not the tool a knob.
- The routed history request is larger than `session_search` made it: 5161
  bytes and 9 tools for the routed-history first request (snapshot
  `routed_history`), against 3673 for a plain turn, which is unchanged. The
  schema in the description is most of the difference; it is what lets a
  model skip an exploratory query.
- Cross-project queries stay out: no `ATTACH`, and only this project's path
  is ever opened. A child agent (#32) must not get these tools; that is the
  child's catalog's job when it lands.
- Revisit if: the payload shape changes so the description's fields go stale
  (the snapshot shows it), or models routinely hit the caps on useful queries.

## Evidence

- #29 (decision 6), #31 (spec and acceptance), the PR closing #31.
- e2e (`crates/construct-dev/tests/e2e.rs`, "history" section): recall of a
  previous session's read after the file changed; `GROUP BY type` rows;
  `DELETE`, `PRAGMA journal_mode=DELETE`, `ATTACH`, `WITH ... DELETE`, two
  statements and `pragma_journal_mode` refused, with the file still WAL and
  its rows intact; an unbounded recursive CTE stopped at 5 s; 100 rows of 1 KB
  cut at 16 KB and 1000 rows cut at 200, each with a note; a history turn with
  an empty `PATH`. `sandbox_keeps_the_journal_out_of_reach` still passes.
- Golden snapshots: `plain_turn` unchanged; new `routed_history`.
- Router (`bench/jev/eval.py --only local`, release builds of main `6628328`
  and this branch): the only router change is dropping "chatstrata" from the
  local History keywords, and no labelled case contains it. Main set groups
  33/47, skills 35/47, gate 13/18 before and after; holdout groups 11/18,
  skills 14/18, gate 5/8 before and after. The Jev questions did not change;
  the Jev half was not run (no key on the machine that made this change).
