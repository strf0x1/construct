# 0008 — The count salt closes stale duplicates under line+hash; hash-only stays unsafe

Status: proposed, 2026-10-21. Accepts 0007's proposal (build the salt as a
knob, rerun); proposes `salt = "count"` as the `[tools.hashline]` default.
0002's decision (the default editor stays `replace`) stands.

## Context

0007 found that a stale anchor on a duplicated line can land an edit on the
wrong copy. The rates were 6/20 runs under line+hash and 11/20 under
hash-only, on two forced-stale tasks. #39 built #26's count salt as
`[tools.hashline] salt = "rank" | "count"` (PR #40).

- `"rank"` is the default and matches the TypeScript reference.
- `"count"` hashes a duplicate as `content + " " + rank + "/" + count`, so
  adding or removing any copy re-hashes every copy.

This ADR is the measurement 0007 asked for: the stale pair and the eight-task
corpus, with four arms on one binary.

## Decision

Proposed:

1. **`salt = "count"` should become the `[tools.hashline]` default**, under
   line+hash. It made 0 wrong-line edits in 20 stale runs, against 6/20
   unsalted. It showed no cost on the corpus. The change gives up
   byte-parity with the TypeScript reference whenever a file has duplicate
   lines, so `matches_the_reference_implementation` stays pinned to `"rank"`.
   Making it the default is a one-line change to `HashlineConfig::default`,
   in its own PR.
2. **`anchor = "hash"` stays a measurement arm, not a recommendation, salt or
   not.** The salt cut stale wrong-line runs from 10/20 to 3/20, then 0/20 in
   a confirmation run. Hash-only has a second failure that no salt can
   reach. In 6 of 10 hash-only `edit-rename` runs, the model paired a line's
   new text with the anchor of the line above, taken from a fresh read. With
   no line number to check, the edit landed. Line+hash: 0 of 10.
3. `editor = "replace"` stays the default (0002).

## Consequences

- With the salt on, line+hash has no known stale-anchor hole on this corpus.
  One case remains open and is pinned in
  `count_salt_still_relocates_when_the_count_is_unchanged`: one copy
  removed and another added between a read and an edit.
- A salted line+hash anchor on a duplicate line no longer matches the
  reference implementation's. Anyone porting anchors across the two must use
  `salt = "rank"`.
- The salt turns a silent wrong-line edit into a loud rejection, and the
  model must re-read and retry. On the stale pair this was free: requests,
  tokens and wall time were all `same`. That holds only for this model.
- What would change this: a model or task where the extra rejections cost
  real retries (a rise in `edit.unrecovered`). Or a corpus with heavy
  blank-line or `}` churn between reads, which this corpus doesn't have.

## Evidence

Every arm in every run below ran binary `7c6f0693…`, built at `4b6139a`
(main after PR #40, clean). The bench tree is `4b6139a`. The corpus and
confirmation results record `dev_dirty`, because the earlier result files
in `bench/results/` were untracked at their start. Model:
`minimax/minimax-m3.1-flash:free`. All runs used `--no-learn` with
interleaved rounds.

| arm | config |
|---|---|
| `hashline` | `editor = "hashline"` (line+hash, rank) |
| `hashline-count` | `editor = "hashline"`, `salt = "count"` |
| `hashonly` | `editor = "hashline"`, `anchor = "hash"` |
| `hashonly-count` | `editor = "hashline"`, `anchor = "hash"`, `salt = "count"` |

A "wrong-line run" is a run whose work dir fails `no_wrong_line` or whose
`tool` records show an `edit.rewrites` (0007's two counts).

### Stale pair

`bench/results/task-20261021-201459.json`: 10 rounds × 2 tasks × 4 arms.

| | line+hash | line+hash + count | hash-only | hash-only + count |
|---|---|---|---|---|
| **issue's shape (n=10)** | | | | |
| wrong-line runs | 0 | 0 | 3 | 2 |
| stale anchor rejected | 7 (`ANCHOR_MISMATCH`) | 5 (`ANCHOR_MISMATCH`) | 0 | 5 (`ANCHOR_NOT_FOUND`) |
| success | 10 | 10 | 9 | 9 |
| requests / wall / prompt tokens (median) | 8.5 / 32 s / 23.1k | 9 / 23 s / 24.8k | 12.5 / 50 s / 36.7k | 11.5 / 39 s / 36.8k |
| **gap shape (n=10)** | | | | |
| wrong-line runs | **6** | **0** | **7** | **1** |
| stale anchor rejected | 1 | 9 | 0 | 5 |
| success | 10 | 10 | 8 | 10 |
| requests / wall / prompt tokens (median) | 12 / 91 s / 39.8k | 10 / 28 s / 28.3k | 12 / 84 s / 37.9k | 10 / 41 s / 26.9k |
| **both (n=20)** | | | | |
| wrong-line runs | 6 | **0** | 10 | **3** |

Every wrong-line run in this result was noticed and fixed by the model:
`no_wrong_line` passed in 80/80 runs. Fisher's exact test on wrong-line
runs:

| comparison | result |
|---|---|
| gap, line+hash vs line+hash + count | 6/10 vs 0/10, p = 0.011 |
| both, line+hash vs line+hash + count | 6/20 vs 0/20, p = 0.020 |
| gap, hash-only vs hash-only + count | 7/10 vs 1/10, p = 0.020 |
| both, hash-only vs hash-only + count | 10/20 vs 3/20, p = 0.041 |
| issue's shape, hash-only vs hash-only + count | 3/10 vs 2/10, p = 1.0 |

`bench.py compare --a <arm> --b <arm>-count`:

- `compare-20261021-213337.json`, line+hash:
  - `edit.rewrites` is `better`: p = 0.020 overall, gap p = 0.011.
  - gap `edit.rejected` and `rereads_after_rejection` are `worse`, and
    `recoveries` is `better` (p = 0.005).
  - requests, tokens and wall time are `same`.
- `compare-20261021-213338.json`, hash-only:
  - `edit.rewrites` is `better`, p = 0.041.
  - `edit.rejected` is `worse`, 0% → 50% of runs, p = 0.0004.
  - `recoveries` is `better`, p = 0.008.
  - everything else is `same` or `inconclusive`.

Every `worse` in these tables is the salt doing its job: a stale anchor
rejected instead of applied. The other `worse` verdicts in all four tables
are `first_request.system_chars`/`bytes` +6. The system prompt carries the
work dir, whose path has the arm name in it, and `-count` is 6 characters.

**The 3 hash-only + count wrong-line runs are not fully explained.** Their
session logs were deleted when the next bench run reset its homes.

- From its transcript tail, one gap run broke `audit(user)`, a line that
  appears once in the file. The model's reply says the first edit landed on
  the wrong line. A line that appears once has the same hash under either
  salt, so this is the model choosing the wrong anchor, not a stale anchor
  resolving.
- The other two edited a line twice. The tails don't say why.

To check, `bench/results/task-20261021-213233.json` reran `hashonly-count`
alone on the pair (10 rounds × 2 tasks, session logs kept):

- 0/20 wrong-line runs;
- stale anchor rejected (`ANCHOR_NOT_FOUND`) in 16/20;
- success 19/20. The failure was a gap run that hit the 300 s turn limit.
  The model decided the edit tool had applied its change to "a stale
  snapshot", reset the file to HEAD, which also removed the teammate's
  block, and ran out of time. This is 0007's investigation cost, and it
  happened on a salted arm.

Pooled, hash-only + count made a wrong-line edit in 3/40 runs, against
10/20 for hash-only (p = 0.0004). Against 0/20 for line+hash + count, the
difference is not significant (p = 0.54).

### Corpus: the salt's cost

`bench/results/task-20261021-211455.json`: 5 rounds × 8 tasks (the `edit-*`
corpus minus the two above) × 4 arms.

| | line+hash | line+hash + count | hash-only | hash-only + count |
|---|---|---|---|---|
| success (n=40) | 35 | 36 | 38 | 36 |
| runs with a rejection | 4 | 4 | 1 (`BAD_ANCHOR`) | 0 |
| edit calls | 51 | 54 | 58 | 54 |
| runs with a rewrite | 1 | 0 | 5 | 1 |
| requests / wall / prompt tokens (median) | 5 / 10 s / 13.2k | 5 / 11 s / 12.9k | 5 / 14 s / 13.9k | 5 / 11 s / 13.0k |

Per task, every arm went 5/5 except:

| task | line+hash | line+hash + count | hash-only | hash-only + count |
|---|---|---|---|---|
| `edit-shifted-block` | 2/5 | 2/5 | 3/5 | 2/5 |
| `edit-duplicate-lines` | 4/5 | 5/5 | 5/5 | 5/5 |
| `edit-whitespace` | 4/5 | 4/5 | 5/5 | 5/5 |
| `edit-stale-reference` | 5/5 | 5/5 | 5/5 | 4/5 |

The shifted-block losses are 0002's trailing-newline failure, which is
still open.

`bench.py compare` on the corpus:

- `compare-20261021-213339.json`, line+hash: no `better` or `worse`
  outside `first_request` (the path) and `edit-rename/prompt_tokens`
  (18.9k → 23.2k, `worse`). Neither rename arm had a rejection; the salted
  arm made more edit calls (16 vs 12). On that file the salt changes only
  the two blank lines, so the difference is the model, not the salt.
- `compare-20261021-213341.json`, hash-only: the only verdict outside
  `first_request` is `edit-rename/edit.rewrites`, `better` (5/5 → 1/5,
  p = 0.048). It is not caused by the salt.

**What the corpus rewrites are.** Classified from the kept session logs
(`tool` records):

- **`edit-rename`, all 6 hash-only runs (both salts).** The model renamed
  in `app.py` from a fresh read, in one step. It sent `vc` (line 4,
  `def report(items):`) with line 5's text, then fixed both lines. The
  salt leaves `vc` and every other non-blank line identical, so the 5/5 vs
  1/5 split is chance. Both line+hash arms: 0/10. Hash-only vs line+hash
  arms: 6/10 vs 0/10, p = 0.011.
- **`edit-whitespace`, 1 line+hash run.** The model wrote `continue` at the
  wrong indent on the right line and fixed it. This is not a wrong-line
  edit: `edit.rewrites` also counts a model correcting its own text.

So no corpus rewrite involved a stale anchor. The salt added no rejection
on the corpus in either anchor mode. The cost 0007 predicted (more stale
anchors when a blank line or `}` changes) did not appear: in this corpus
no file changes between a read and an edit except `edit-stale-reference`,
which has no duplicates.

### Tooling found along the way

- `compare`'s mostly-zero count rule (#40) also caught `prompt_tokens`
  with one empty run. It now applies only when one side's median is 0.
- `bench.py note` now prints the salt next to the anchor.
- `fresh_homes` deletes a suite's previous homes, so chaining two `task`
  runs loses the first run's session logs. Copy them out, or give each run
  its own `BENCH_WORK`.

Caveats: one free model; n=10 per task on the pair (plus 20 confirmation
runs for one arm), n=5 per task on the corpus; small synthetic files.
