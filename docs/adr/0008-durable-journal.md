# 0008 — A SQLite journal per project, holding the one Event type

Status: accepted, 2026-10-21. Implements #30; the decisions are #29's (2, 3, 4,
7 and 8), not re-argued here.

## Context

Sessions were append-only JSONL files, one per session, under
`$CONSTRUCT_HOME/sessions/`. They wrote their own row shapes (`session`,
`message`, `usage`, and `event(kind, data)` for route, tool and compact) next
to `agent::Event`, which carried only UI and stream events. Two vocabularies
for one run: `--json` could not be replayed into a session, the session log
could not be compared to `--json`, and `run -c`, the learn debounce (file
size) and the bench each parsed the files their own way.

#29 settled what replaces them: JSONL stays the wire, SQLite WAL is the
record, export is a dump; one journal per project kit; the parent is the only
writer; the journal is not a compaction strategy; additive-only payloads,
`PRAGMA user_version` for the table shape, readers skip what they don't know,
and a small projector rebuilds context for resume. This ADR records how that
landed and what it cost.

## Decision

- **One `Event` type** (`crates/construct/src/event.rs`) is what the agent
  emits, what `--json` prints, and what a row's `payload` holds. It gained
  `head` (cwd, model, manifest, version; optional `parent_session` and
  `parent_call_id` for #32), `system`, `user`, `assistant` and `tool_result`
  (each carrying the `llm::Message` verbatim; `tool_result` also carries the
  call `id`), and `request` (model, `bytes`, `n_tools`, tool names: the
  numbers the golden snapshots pin, for the body actually sent). From here on
  the enum is additive-only: new variants, new fields with `#[serde(default)]`.
- **The journal** is `$CONSTRUCT_HOME/projects/<id>/journal.sqlite`, `<id>`
  from `project::detect` (a directory outside any repo gets an id from its
  path). One table, `events(seq INTEGER PRIMARY KEY, session_id, type, v,
  at_ms, payload)`; indexes on `(session_id, seq)`, `type` and
  `json_extract(payload, '$.id')`; an FTS5 external-content table over the
  `user`, `assistant` and `tool_result` text (not the system prompt, which
  repeats per session), kept in step by triggers. WAL, `synchronous=NORMAL`,
  a 10 s busy timeout, and a retry around the first WAL switch and migration,
  which can report busy without waiting when sessions create the file at once.
- **Every durable event is journaled on the way out.** `journal::Writer::emit`
  appends the row and sends the event under one lock, so rows and the stream
  agree on order when tools finish in parallel. Stream deltas (`opened`,
  `text`, `reasoning`, `tool_pending`) and the TUI's `learn` status are not
  kept: the assistant message holds the text, and a row per token would be
  most of the file. Rows therefore equal `--json` minus those four types; an
  e2e test pins exactly that.
- **Readers**: `since(session, seq)`, `last_seq`, `latest(cwd)` and
  `project()`, which turns message-bearing events back into `Vec<Message>`
  and nothing else. Rewind is `rewind(session, seq)` (delete after seq) then
  re-project. `run -c` resumes the newest session whose head names this
  directory, within this project's journal (clones and worktrees share the
  file, but "continue" stays per directory, as the flag's help says). The
  learn debounce is "last seq unchanged for N ms"; learning reads
  `since(session, reviewed_seq)` (state key `reviewed_seq`, a seq, replacing
  the message count `reviewed`).
- **Export**: `construct sessions export <id>` prints the old JSONL shape
  (`session`, `message`, `usage`, `route`, `tool`, `compact`), so chatstrata's
  `construct` source and old tooling keep working. The bench reads journals
  with the stdlib `sqlite3` through `bench/journal.py`, which produces the same
  shape, so `process.py`'s edit outcomes still come from the `tool` records
  (now `tool_end` events) and homes from before the journal still read.
- **Dependency**: `rusqlite` 0.40 with `bundled` (SQLite compiled in, FTS5 and
  JSON functions included), pinned in `[workspace.dependencies]`. No system
  libsqlite3, one C compile, +2.33 MB of release binary (Evidence). DuckDB
  stays outside the binary.
- **Sandbox**: the journal sits under `$CONSTRUCT_HOME/projects/`, outside
  every root a tool gets (the working directory, the private `tmp/<pid>`, the
  spill dir, system paths and toolchains), so neither a shell command nor a
  file tool can read it. An e2e test pins both.

## Consequences

- `--json`, the wire to a future child (#32), and the record are one parser.
  A test can deserialize either as `Event` and compare.
- Concurrent sessions in one repo share a file safely; a second project gets
  its own, which is the only history its sandboxed agent could ever be shown
  (#31).
- Payload bloat is bounded by not journaling deltas, but `tool_end.output`
  and `tool_result.message` hold the same text twice. Acceptable now; since
  dropping a field is not additive, changing that is a new ADR.
- Rows are written synchronously on the agent's task. Each is one small
  insert in WAL mode with no fsync; the tier 1 numbers below are the check.
- `sessions/` is no longer written. Anything that read it must use the
  export or the journal.
- Revisit if: payloads need a breaking change (bump `v`, upcast in
  `journal/read.rs`), or a single journal per project becomes a contention
  point (it would show as busy waits in many-session runs).

## Evidence

- #29 (decisions), #30 (spec and acceptance), PR closing #30.
- e2e (`crates/construct-dev/tests/e2e.rs`, "journal" section): rows after a
  scripted turn, rows equal to `--json`, `run -c` sends what a JSONL resume
  sent, four concurrent sessions across two clones plus a second project,
  export round-trip, and the sandbox denying `journal.sqlite` to shell, read
  and grep.
- Golden request snapshots unchanged: what the model sees did not move.
- Tier 1, main `4b6139a` vs `e19c29d`, release builds in separate checkouts:
  - `tools` (`bench/results/tools-20261021-191705-main.json` vs
    `tools-20261021-191928.json`, `compare-20261021-192033-1.json`): read p50
    0.413 → 0.477 ms, shell p50 2.35 → 2.46 ms, wall 72.1 → 75.6 ms; every
    verdict `same`. A journal row per event does not show at this resolution.
  - `size` (`size-20261021-192006-main.json` vs `size-20261021-192027.json`;
    one sample each, so `compare` has nothing to judge): binary 17.25 MB →
    19.58 MB (+2.33 MB, +13.5%), more than the issue's "roughly a megabyte";
    a fresh home after one run 2 KB → 37 KB (the journal and its WAL).
