# 0002 — The default editor stays `replace`; hashline stays an arm

Status: accepted, 2026-10-21. The count-salt deferral and "line+hash … never wrong" are superseded by 0007; the decision (default stays `replace`) stands.

## Context

Issue #1 asked for interchangeable tools so editors can be A/B'd. Proposal 04
built the catalog and the hashline port (#23, #24), plus hash-only anchors as a
knob (#25). Step 6 is the first paired run of three arms on the same tasks and
model, in interleaved rounds, with learning frozen:

| arm | config |
|---|---|
| `replace` | `editor = "replace"` (old_string/new_string) |
| `hashline` | `editor = "hashline"`, `anchor = "line+hash"` |
| `hashonly` | `editor = "hashline"`, `anchor = "hash"` |

The corpus is eight small tasks in `bench/tasks/edit-*`: duplicate lines,
whitespace only, Unicode, a block shifted by an insertion above, a stale
reference (the file changes between turns), issue #26's duplicate relocation,
a multi-file rename, and one line deep in a 400-entry file. Each is scored
byte for byte against `expected/`, never from the transcript.

## Decision

`editor = "replace"` stays the default. `hashline` (both anchor modes) stays
available as a config arm. The hash-only count salt from issue #26 is not
built: this run never produced a wrong-line edit, so there is nothing for it to
fix yet.

## Evidence

Run: `bench/results/task-20261021-121125.json`. Its manifest records:

- **construct:** `6ea1f93`, dirty. The uncommitted change was the session-log
  `tool` record, committed in `e639095` and changed only by `cargo fmt` since.
  All three arms ran the same binary, `5e710eff…`.
- **bench:** `e442b69`.
- **model:** `minimax/minimax-m3.1-flash:free`.
- **runs:** 5 rounds × 8 tasks × 3 arms = 120, with `--no-learn`.

PR: #28. `bench.py compare … --a replace --b <arm>`:

| | replace | hashline | hashonly |
|---|---|---|---|
| success | **40/40** | 37/40 (p=0.24 vs replace) | 39/40 (p=1.00) |
| wrong-line edits (duplicate tasks, n=10) | 0 | 0 | 0 |
| unintended file changes | 0 | 0 | 0 |
| edit calls | 69 | 54 | 58 |
| edit rejections | 0 | 4 (`ANCHOR_MISMATCH`, all on the stale-reference task) | 0 |
| recovered (applied within 2 steps) / re-read first | – | 4/4 / 4/4 | – |
| first request, prompt tokens (median) | **1372** | 2180 (+808, `worse`) | 2122 (+750, `worse`) |
| prompt tokens per run (median) | **9174** | 13760 (+50%, `same` overall, `worse` on shifted-block) | 12710 (+39%, `same` overall) |
| completion tokens per run (median) | 482 | 504 | 510 |
| requests per run (median) | 5 | 5 | 5 |
| wall time per run (median) | 9.0 s | 9.4 s | 9.0 s |

Per task, every arm went 5/5 except `edit-shifted-block`: replace 5/5,
hashline 2/5, hashonly 4/5.

What the numbers say:

- **No accuracy win to pay for.** On this model and corpus, replace never
  failed. The hashline arms lost four runs, all one way. On the shifted-block
  task the model inserted a docstring that should end in a blank line. In
  the failing runs its `text` ended in a single `\n`, which hashline reads as
  the line terminator, so the blank line was never written. The passing runs
  sent `\n\n`. Under replace, the blank line is literal text in `new_string`,
  so this mistake can't happen. It is a protocol cost, not a construct bug: the
  TS reference behaves the same.
- **A fixed token cost.** The hashline tool descriptions add ~750–800 prompt
  tokens to every request, and anchored read output is bigger. That comes to
  +39–50% prompt tokens per run at equal request counts.
- **line+hash does what it promises.** On the stale-reference task (the file
  changes between turns), 4 of 5 line+hash runs had the stale anchor rejected,
  re-read, and landed the retry: loud, never wrong. hash-only followed the moved
  content and applied first time in 5 of 5. That is the retry hash-only saves.
- **Issue #26's hazard was not exercised.** The model made both edits of the
  duplicate-relocation task in one call in 9 of 10 hashline runs. Anchors from a
  single read are never stale, so 0 wrong-line edits here says nothing about
  relocation risk.

Caveats: one free model, n=5 per task. No rate difference is significant at
p < 0.05. Only the token costs are clear-cut.

## Consequences

- Prompt and schema work keeps targeting the replace editor.
- To revisit, we need one of three things: a model where replace fails
  (`not_found`/`ambiguous`) and hashline doesn't, a stronger model on the same
  corpus, or a cheaper hashline description.
- Before #26's count salt is judged, the relocation task must force a stale
  anchor (a second turn after the copy is added, as in `edit-stale-reference`).
- The hashline `text` trailing-newline rule should be stated in the edit tool's
  description, then the shifted-block task rerun.
