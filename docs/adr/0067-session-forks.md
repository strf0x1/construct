# 0067 — Session forks branch by reference, never truncate

Status: accepted. 2026-11-22. Closes #247 step 2 (under #243); takes up the
"session forks" decision deferred by ADR 0029 and the append-only rule of
ADR 0008. Implementation follows in #369.

## Context

`Journal::rewind` deleted every row after a seq. Its only caller was its own
unit test; it broke the append-only promise that makes the journal the record
of a run, and it was a trap for whoever built "undo" on top of it. #247
removed it (PR #322), leaving the question: how does a user branch a
conversation that went off the rails without destroying what happened?

pi-durable (`earendil-works/pi@cead615`, `packages/durable`) answers with
forks: a new conversation pointing at one visible entry of the parent; reads
walk the child's entries, then the parent's through that cap; mutable state is
copied per a persisted policy (`asOf` / `current` / `initial`); tasks are never
inherited; a cut landing mid-tool-call synthesizes an error result, never a
re-run. The full source check is on
[#247](https://github.com/strf0x1/construct/issues/247#issuecomment-7047319382).

ADR 0029 already names session forks as a separate decision and fixes the
ground rules inherited here: restore only while idle, exact directory, fresh
tools/sandbox/permissions, and selecting a session never retries a tool or
starts generation. Two later changes sharpen the record shape: ADR 0055
removed delegation, so `Head.parent_session`/`parent_call_id` are now read-only
legacy fields that only describe pre-#294 child sessions; ADR 0063 showed that
a changing tool list misses the prompt cache, which makes a fork's new prefix
a cost to acknowledge, not an accident.

## Decision

A fork is a new session in the same project journal. It references the
parent's rows; it never copies or deletes them.

- **Head shape.** `Head` gains two additive optional fields: `fork_of`
  (parent session id) and `fork_seq` (the parent's `i64` seq the fork
  continues from), both `#[serde(default, skip_serializing_if =
  "Option::is_none")]`. `parent_session`/`parent_call_id` keep only their
  legacy reading meaning under ADR 0055 — nothing writes them, and a fork has
  neither. Reusing them for forks would either hide forks from the browser or
  surface old child sessions as sessions. The schema stays additive-only
  (ADR 0008); no migration.
- **Projection.** A fork's conversation is the parent's events through
  `fork_seq` — recursively when the parent is itself a fork — followed by its
  own. `Journal::since`, `last_seq` and raw `journal_query` SQL stay per-session
  rows; a fork-aware read path feeds `context::replay`, `journal::project`,
  browser labels, `journal::show::transcript` and `jobs::resume`, so inherited
  `system` and `compacted` events apply where they happened. The cut is a
  per-row seq, unlike pi's whole-commit document state: there is no
  same-commit ambiguity to resolve.
- **Cut repairs.** When the cut leaves a tool call without its result, an open
  turn, or a job still open, the same pairing recovery uses
  (`journal/recovery.rs`) synthesizes the interrupted result and closes the
  turn. Repair rows are journaled under the fork's own id, never the parent's.
  No tool is re-run (ADR 0029); pi does the same at a fork cut.
- **Browser and `-c`.** Forks are conversations the user started, so the
  browser lists them: its `parent_session IS NULL` filter keeps hiding old
  child sessions without change, and a fork head has no `parent_session`.
  Labels read fork-aware history, so a fresh fork shows the parent's original
  prompt or name plus a fork marker; the "at least one original prompt" rule
  counts inherited prompts. `Journal::latest` and `run -c` treat a fork like
  any session — the newest head naming this directory wins. A fork stays in
  the parent's project and cwd: the journal is per-project and ADR 0029's
  exact-directory rule applies. Cross-project forks are out of scope.
- **Jobs.** None inherited. `jobs::resume` reads the fork's own rows only, so
  the parent's jobs — live or finished — stay with the parent's runtime
  (ADR 0026: jobs die with the process; pi: tasks are never copied). Parent
  job rows project as history; a job still open at the cut is shown
  interrupted by the cut repairs above.
- **Permissions.** Fresh, exactly as switching sessions today (ADR 0029):
  session grants are per-process memory, not journal rows, so a fork starts
  with none and re-asks; config grants apply as always. Nothing is copied
  from the parent's grant state. This is pi's `initial` policy and is already
  construct's behaviour on resume.
- **Manifest and model.** The fork's head records the manifest and model
  current at fork time (pi's `current`), like any new session. Inherited
  history may reference tools the fork no longer has; history is history.
  Tool groups still only grow within the fork (ADR 0005), and the fork is a
  new prompt-cache prefix, so nothing in the parent is invalidated. ADR 0063
  records that a new prefix costs a cache miss on the first fork request;
  that cost is accepted here.
- **Creation.** The default cut is the parent's `last_seq` — "branch from
  here". Creation goes through the same idle-only, ownership-locking path as
  restore. The UX seam (browser action / slash command, CLI verb) belongs to
  the implementation issue; this ADR fixes the record shape, not the
  keybinding.

## Consequences

Branching becomes non-destructive: the original run stays whole and auditable,
nested forks compose by chained caps, and no new storage or migration is
needed. "Undo" is a fork plus a switch, never a delete.

Reads walk the fork chain, so a deep chain costs restore time; the #246
restore-cost measurement applies to chains as well as long sessions. Recall
is fork-aware where the conversation is (projection, browser labels); raw
`journal_query` SQL stays per-session, with the parent reachable by id.

Pre-fork builds skip the unknown head fields and project only the fork's own
rows: history looks truncated, nothing crashes, and their `-c` may pick a
fork. Additive-only makes this the accepted degradation, not a wire break.

This rules out any destructive undo on the journal and any re-run of a cut
tool call. Revisit if measured restore cost grows with chain depth, or if
Deck guests need forks across machines (the journal is per-project today).

## Evidence

- pi-durable source check, pinned `earendil-works/pi@cead615`:
  https://github.com/strf0x1/construct/issues/247#issuecomment-7047319382.
  Transcript references the parent's visible entry; document state follows
  per-document `asOf`/`current`/`initial`; tasks are never copied; a fork cut
  synthesizes a missing tool result instead of re-running it.
- Rewind removal: PR #322. Journal unit tests 15/15, e2e 275 passed, golden
  snapshots unchanged; feature-map recipes `delegate-progress` and
  `session-browser` re-driven at the code commit and bumped in a map-only
  commit.
- Issues: #243 (parent), #247 (decision), #369 (implementation).
- ADRs 0005, 0008, 0026, 0029, 0055, 0063.
