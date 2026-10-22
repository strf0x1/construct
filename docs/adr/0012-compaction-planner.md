# 0012 — Compaction cuts rounds: stub old results first, summarize second

Status: accepted, 2026-10-22. Implements proposal 01 steps 1–2. Builds on
0008 (durable journal) and #29 decision 7 (the journal is not a compaction
strategy).

## Context

Compaction summarized history once the last prompt passed 75% of
`context_window`, cutting at the second-to-last user message. A single-prompt
run has one user message, so it never compacted: every `construct run`, every
bench task and every future #32 child grew until the provider rejected it.
`compacted` journaled only message counts, so resume replayed the full
history. A provider "too long" rejection ended the turn.

The design ported substrate_v3's planner (`compaction-features.md`), minus
recall; proposal 01 lists every feature kept, deferred and dropped.

## Decision

- **The round is the unit**: an assistant message and its tool results. A
  cut never separates a call from its result.
- **Plan before every request** from the size of the real request body,
  converted with bytes-per-token measured on the provider's own counts. Over
  `threshold` (0.7) of the budget (window less output reserve): stub tool
  results outside the last `keep_results` rounds; if that misses `target`
  (0.5), stub all but the newest round; if that misses too, summarize older
  rounds, keeping as many recent ones as fit. The user's latest request is
  pinned verbatim under a summary. The lower candidate wins when none reaches
  the target.
- **Overflow**: a provider rejection that says the prompt was too long forces
  a cut and one resend, and a stated window ("maximum context length is N")
  replaces the configured one for the session.
- **The cut is journaled** (`compacted.cut`) and `compact::apply` replays it
  in the projector, so resume sees what the model saw.
- **Summarizer**: the run's model unless `[compaction] model` is set, 8192
  tokens of reasoning headroom over the summary allowance, one retry on an
  empty answer (added after the baseline run below, which saw two empty
  summaries in one `k4` run), and a summary that ends on `length` is refused (the cut falls
  back to stubbing, the event says why, and the tokens are still counted).
- Defaults `threshold = 0.7`, `target = 0.5`, `keep_results = 2`,
  `keep_rounds = 4`, `age_rounds = 0` (off) stand until a sweep settles them.

## Consequences

- Long single turns, and #32 children, can run past the window.
- Every cut costs prompt cache from the first changed message on: the request
  after a cut hit cache at 0.35 against 0.84–0.86 overall. #51 should judge
  breakpoint placement with `compaction.cache_hit_after_cut`.
- Stubs are re-runnable, and the model does re-run them: a median of 3 calls
  per run. Fewer, later cuts may cost less than the stubs save; that is the
  next sweep.
- Revisit `keep_results` (see Evidence: 4 looked better than 2 but not
  significantly) and the threshold (`main` worked to 0.97 of the window
  without trouble when it didn't overflow).

## Evidence

`bench/results/task-20261022-053503.json`, `minimax/minimax-m3.1-flash:free`,
32k window enforced by the endpoint, 3 rounds × 3 tasks per arm, no runs
invalidated by provider failures. `main` = `3b16106`; `new` = this branch;
`k4` = `new` with `keep_results = 4`.

| | main | new | k4 |
|---|---|---|---|
| success | 6/9 | 9/9 | 9/9 |
| compact-recall success | 0/3 | 3/3 | 3/3 |
| runs with a fatal overflow | 6/9 | 0/9 | 0/9 |
| peak fill (median) | 0.97 | 0.60 | 0.60 |
| prompt tokens (median) | 328k | 605k | 369k |
| requests (median) | 21 | 42 | 28 |
| wall s (median) | 271 | 657 | 416 |
| stub re-runs (median) | 0 | 3 | 3 |
| cache hit, request after a cut | 0 | 0.35 | 0.25 |

`bench.py compare --a main --b new`: overflows and overflow errors `better`
(p=0.01), peak fill `better`, cache hit after a cut `better`, stub re-runs
`worse` (p<0.01); success 67% → 100% `inconclusive` (p=0.21) pooled, 0/3 →
3/3 on compact-recall (p=0.10); prompt tokens, requests and wall time `same`
pooled (wide spread) but `worse` on compact-recall, where `main`'s turns died
early. Against the losses: on long-fix and conventions `main` passed by
running to the edge of the window, at about half the tokens. `--a new --b k4`:
every metric `same`; k4's medians are lower on tokens (369k vs 605k),
requests and wall time.

Pilots that shaped the design (proposal 01, "What the pilots changed"): a 20k
window failed outright on thrash and empty summaries, fixed by escalating
stubs before summaries and by the summarizer headroom; one round at 32k
passed all three tasks (`task-20261022-010543.json`).
