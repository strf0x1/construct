# 0007 — Stale anchors on duplicate lines: hash-only needs the count salt, line+hash has a hole too

Status: proposed, 2026-10-21. Supersedes the count-salt and "never wrong"
parts of 0002; 0002's decision (the default editor stays `replace`) stands.

## Context

Issue #26: a duplicate line's hash is salted with its rank among identical
lines, so when a new copy appears above the copies, a stale hash-only anchor
silently names a different copy. 0002 deferred the fix because its run never
exercised the hazard. The model made both edits of `edit-duplicate-relocation`
in one call, so no anchor was ever stale. 0002 also said line+hash is "loud,
never wrong".

Two new tasks force the stale anchor the way `edit-stale-reference` does.
Turn 1 is a read. Between turns a hook adds a block above the target that
holds another copy of it (`return None`). Turn 2 asks for an edit to the final
copy.

- `edit-stale-duplicate`: the block is three lines, the issue's shape.
- `edit-stale-duplicate-gap`: the block is two lines, as tall as the gap
  between two existing copies. The previous copy then lands on the stale line
  number with the stale rank, so a stale `line:hash` anchor checks out and
  names the wrong copy. `anchors.rs` pins this:
  `line_hash_accepts_a_stale_duplicate_when_the_insert_matches_the_gap`.

A wrong-line edit is counted two ways:

- the work dir's check (`no_wrong_line`: both early-exit branches intact);
- `edit.rewrites` from the session's `tool` records: an applied edit over a
  line an earlier applied edit wrote. This is the trace of a wrong-line edit
  that the model noticed and fixed.

## Decision

Proposed:

1. `replace` stays the default. It made no wrong-line edit in 20 runs; the
   hashline arms made them in 6/20 (line+hash) and 11/20 (hash-only).
2. Build #26's count salt (`content + " " + rank + "/" + count` for
   duplicates) as a knob under `[tools.hashline]`, so it can be measured as an
   arm. Adding or removing a copy then changes every copy's hash, and a stale
   anchor fails loudly. Apply it to both anchor modes: in line+hash it closes
   the gap hole, at the cost of diverging from the TypeScript reference.
3. Rerun these two tasks and the eight-task corpus with the salted arms
   before any hashline mode is recommended. The salt's cost is more stale
   anchors whenever a blank line or `}` is added, and the corpus measures
   that.

## Consequences

- Until the salt lands, `anchor = "hash"` is not safe on files with
  duplicated lines that change between reads. `line+hash` is safe except in
  the gap geometry.
- The salted knob makes line+hash anchors differ from the reference
  implementation's whenever duplicates exist, so `matches_the_reference_implementation`
  must stay pinned to the unsalted mode.
- What would change this: a rerun where the salted arms show no wrong-line
  edits and no rise in rejections on the corpus.

## Evidence

Run `bench/results/task-20261021-172610.json`. Its manifest records:

- **bench tree:** `e34fbe1`, clean, with `36f2313` as the same tree rebased
  onto main.
- **binary:** `5e710eff…`, built at `6ea1f93` with the tool-record change
  uncommitted (see 0002). It is the same binary as 0002's run, used by all
  arms. This branch's only Rust change is a unit test.
- **model:** `minimax/minimax-m3.1-flash:free`.
- **runs:** 10 rounds × 2 tasks × 3 arms, interleaved, with `--no-learn`.

PR: this one. Issue #26.

| | replace | line+hash | hash-only |
|---|---|---|---|
| **issue's shape (n=10)** | | | |
| runs with a silent wrong-line edit | 0 | **0** | **7** (all noticed and fixed) |
| stale anchor rejected (`ANCHOR_MISMATCH`) | – | 6 | 0 |
| success | 10 | 10 | 10 |
| requests per run (median) | 12.5 | 9.5 | 12.5 |
| wall time per run (median) | 64 s | 36 s | 60 s |
| prompt tokens per run (median) | 30.6k | 28.0k | 36.5k |
| **gap shape (n=10)** | | | |
| runs with a silent wrong-line edit | 0 | **6** (4 fixed, 2 left in the file) | **4** (2 fixed, 2 left) |
| stale anchor rejected | – | 2 | 0 |
| success | 9 (1 over budget, edit correct) | 8 | 8 |
| requests per run (median) | 15 | 11 | 13 |
| wall time per run (median) | 138 s | 92 s | 154 s |
| prompt tokens per run (median) | 69.3k | 35.3k | 43.8k |

Fisher's exact test on runs with a silent wrong-line edit (`compare.fisher`):

| comparison | result |
|---|---|
| issue's shape, line+hash vs hash-only | 0/10 vs 7/10, p = 0.003 |
| gap shape, replace vs line+hash | 0/10 vs 6/10, p = 0.011 |
| gap shape, replace vs hash-only | 0/10 vs 4/10, p = 0.087 |
| both tasks, replace vs line+hash | 0/20 vs 6/20, p = 0.020 |
| both tasks, replace vs hash-only | 0/20 vs 11/20, p = 0.0001 |

These are the first significant differences in the editor experiment.
`bench.py compare` calls the `edit.rewrites` difference `same`: the spread
rule (p10–p90) can't separate per-run counts that are almost always 0 or 1.
That is a known gap in `compare`, so the rates above are computed directly.

What the runs show:

- **The wrong text, too.** A stale edit carries the text meant for the final
  line, at the final line's indent. Landing in a branch, it breaks the file:
  the result doesn't parse. All four hashline failures on the gap task are
  this. The model saw the broken file and spent the rest of the 300 s turn
  investigating instead of fixing it.
- **Noticed only by luck.** Fixes happened when the model re-read or diffed
  the file afterwards. The edit result reported success. Its "fresh anchors"
  window showed the region it had actually changed, which was the only clue.
- **Replace's cost is different.** Replace has no staleness signal, so the
  model meets an unexplained change. In this run it investigated, at a median
  of 138 s and 69k prompt tokens on the gap task, but kept the teammate's
  block in 20/20 runs. In a pilot of the same tasks (killed at 57/60 by a tool
  time limit, never saved), replace deleted the teammate's block in 4 of 9 gap
  runs. That pilot also used an earlier `no_wrong_line` that looked only for
  the branch indent and missed the realistic case; the result above uses the
  fixed check.

Caveats: one free model, n=10 per task, and two small synthetic files.
