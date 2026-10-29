# 0025 — Silent provider conditions are `Note` rows, coalesced per model call

Status: accepted. 2026-10-29.

## Context

The provider client skips or recovers from many conditions without telling
anyone: an SSE line that isn't JSON is dropped, a 429 is retried after a sleep,
a stream that ends without a finish reason returns `Ok`, a tool-call id that
changes at an index is merged into the old call, a 400 naming `max_tokens`
looks like any other 400. #47 lists the gaps; nothing could say which of them
fire in practice, or compare providers by how often. The journal is the record
of a run (AGENTS.md), so the answer goes there, but a flaky stream can produce
hundreds of occurrences and the journal has to stay readable by eye (#48).

## Decision

- **One event, many kinds.** `Event::Note { class, kind, detail, count }`.
  `class` is the subsystem (`provider` today; `tool`, `mcp`, `learn` later),
  `kind` the condition. A new category adds `Kind` constants in the module that
  owns it (`llm::notes`), not an `Event` variant, so the journal schema, the
  projector and `bench/journal.py` don't change.
- **One coalescer holds every anti-spam rule** (`journal::notes::Coalescer`).
  Call sites say `notes.record(KIND, detail)` and nothing else; they never
  format, truncate, count or decide whether to emit. `drain` yields one `Note`
  per kind at the end of a model call (success, failure or cancel); a call that
  recorded nothing yields nothing. Detail is the first occurrence's, one line,
  300 bytes at most.
- **Per-session cap per kind** (`Kind::cap`, 20 rows). Past it the occurrences
  are counted, and the turn's end writes one `suppressed` row per kind whose
  `detail` names the kind it folds. A session has no end hook, so the turn end
  is where it is written; a long flaky session gets one such row per turn, not
  one per occurrence.
- **Journal-only.** The TUI ignores `Note`; `Event::Error` stays the signal for
  fatal cases. `--json` prints it like every other event, and `sessions show`
  renders a line.
- **Reporting is a script, not harness-side posting.**
  `bench/journal.py errors --since 7d [--provider X] [--model Y]` summarizes
  notes, errors and failed tool calls by provider dialect and model, as rates
  per 100 turns. The harness never posts to GitHub: that would put a token in
  the process the model's tools run beside, publish error text that can hold
  prompts and paths, and spam on a flaky network.
- First kinds: `retry`, `bad_chunk`, `truncated`, `index_collision`,
  `param_rejected`.

## Consequences

- Behaviour is unchanged: notes observe, they don't fix. The fixes are #47,
  prioritised by what the report says.
- Rows come out ordered by kind name within a call, not by when each first
  happened; `at_ms` and the surrounding `request`/`usage` rows place them.
- `retry` keeps the first status and the attempt count, not total wait time.
- Calls made through `Client::complete` (learning, the compaction summarizer)
  drop their notes: they have no journal writer to flush to. Revisit if their
  failures matter.
- `bad_tool_args` is not a kind: tool failures already have `tool_end` with an
  error code, and the report reads those as they are.
- Not done here: carrying the counts in tier-2 bench results so `bench.py
  compare` can judge a hardening change by "bad chunks / 100 turns".

## Evidence

`crates/construct-dev/tests/notes.rs` drives the real binary against the
scripted endpoint (503s, garbled chunks, a cut stream, a rejected parameter)
and reads the rows back; `journal::notes` and `llm::stream` unit tests cover
the coalescer and the index collision; `bench/test_journal.py` covers the
report. Issue #48.
