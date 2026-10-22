# Proposal 1 — Context window management

Status: steps 1–2 built and step 3's baseline run on `claude/compaction`
(ADR 0013); the policy sweep is next. Written 2026-10-21 against `3b16106`.

## Where it stood

`agent.rs` summarized history once the last prompt passed 75% of
`context_window`, cutting at the second-to-last user message. Two defects
followed from that one choice:

1. **A single long turn never compacted.** With one user message the cut
   index was 1, which the code refused. Every `construct run "…"`, every
   bench task and every future #32 child is a single-prompt run, so a long
   task grew until the provider rejected it and the run died.
2. **Resume replayed the uncompacted history.** `compacted` journaled only
   message counts, and the projector rebuilt context from every message
   event, so `run -c` after a compaction sent the full original history.

Also: the trigger read the previous request's count, blind to a large tool
result appended since; the summarizer saw 400 chars of each tool result
(`learn::digest`), which is where the facts are; nothing recovered from a
provider "too long" rejection; and nothing measured whether compaction
worked.

## What we took from substrate_v3

substrate_v3 ran the same experiment in TypeScript (`compaction-features.md`,
merged from its proposals 10 and 11). Its finding that agents rarely use a
recall tool on their own is #29 decision 7 here: compaction must be correct
with nobody reading the journal. Feature by feature:

| Feature | Verdict | Why |
|---|---|---|
| Round as the cut unit (call never split from result) | **kept** | fixes defect 1 |
| Planner: elide old results first, summarize only if that misses the target | **kept** | eliding costs no model call and keeps the prefix up to the first stub |
| Threshold 0.7 / target 0.5 of the budget | **kept**, as defaults | settings, swept by arms |
| Stubs with a one-line gloss | **kept, simplified** | deterministic: size and first line, no summarizer-written gloss |
| Summary from originals, structured (Goal / Progress / Facts / Open / Next) | **kept** | tool output gets 2000 chars each in the summarizer's input, not 400 |
| Build on the previous summary | **kept** | a prior summary always leads the summarizer's input |
| Overflow recovery: force a cut, resend once | **kept** | plus: the provider's stated window is adopted for the rest of the session |
| Size the next request, not the last | **kept** | the planner sizes the real request body |
| Measured chars per token, request overhead counted | **kept, simplified** | bytes of the whole body per provider token, so tools and system are in it |
| Output reservation | **kept** | `max_tokens`, or an eighth of the window up to 8192 |
| Policy parameters as settings, recorded in the manifest | **kept** | `[compaction]`; the manifest carries it and `context_window` |
| `elapsedMs` and the cut on every `compacted` event | **kept** | plus summarizer usage, estimate before/after, `reached`, `fallback` |
| Scorecard over journals | **kept** | `bench/compaction.py` |
| Aging (elide past N rounds regardless of size) | **arm, off by default** | every age cut moves the cached prefix; measure against #51 first |
| Prefer a cut over none | **kept** | the lowest candidate is taken and journaled `reached: false` |
| Recent window as a share of the window, floored by turns | **kept, simplified** | as many of the last `keep_rounds` as fit the target, at least one |
| Pin the user's request | **added** | the latest request rides verbatim under the summary instead of being paraphrased |
| Warm summarizer (reuse the run's cached prefix) | **later** | only pays with prompt caching (#51); an arm once that lands |
| Soft line | dropped | aging off by default makes it moot |
| Replay of the summarizer request | dropped | nothing reads it |
| Recall as part of the strategy | dropped | #29 decision 7; `recall_result` stays a history tool |
| Background summarization, manual compaction, handoff/reset, summary trees | dropped | exploratory in substrate_v3; nothing here to measure them with yet |

## Design

`crates/construct/src/compact/` is the module. The model's context is a view
of the journal: a cut changes only the view.

- **Plan.** Before each request the agent sizes the request body for the
  current messages and tools, in tokens via the measured bytes-per-token.
  Over `threshold × budget` (budget = window − output reserve), candidate A
  stubs every tool result outside the last `keep_results` rounds; if that
  reaches `target × budget` it is the cut. Otherwise candidate B summarizes
  everything before the last `keep_rounds` rounds (fewer if they don't fit),
  with a token allowance for the summary. If neither reaches the target, the
  lower one is taken.
- **Cut.** `Cut { elide: [call ids], summary: { upto, text, request } }` is
  journaled on the `compacted` event and applied by `compact::apply`, which
  the projector also calls, so resume sees exactly what the model saw.
- **Overflow.** A rejection whose text says the prompt was too long forces a
  plan at any size and one resend; "maximum context length is N" lowers the
  window for the rest of the session. A second rejection ends the turn.

Config, with defaults:

```toml
context_window = 131072
[compaction]
threshold = 0.7      # of the budget: when to cut
target = 0.5         # what a cut must reach
keep_results = 2     # recent rounds whose results are never stubbed
keep_rounds = 4      # recent rounds kept verbatim under a summary
age_rounds = 0       # stub results older than this many rounds; 0 = off
model = ""           # summarizer; empty = the run's model
```

## Measuring it

Tier 0 pins the mechanics with a scripted model whose window is enforced
(`construct-dev serve --window N` answers `context_length_exceeded` past N):
`crates/construct-dev/tests/compaction.rs` covers the long single turn,
overflow recovery with one resend, summary with the request pinned and the
second summary built on the first, and resume from the cut.

Tier 2 is the compaction suite: tasks with `window = 32000` in `task.toml`,
which both caps the proxy (the provider's own count decides) and sets
construct's `context_window`. Generated by `bench/compact_fixtures.py`:

| Task | What it stresses |
|---|---|
| `compact-long-fix` | one prompt, six bugs across a ~1.8k-line package: a long single turn |
| `compact-conventions` | rules read first (random code prefix, suite marker, ticket) applied after reading the whole package |
| `compact-recall` | four turns; the turn-1 answer's source is deleted, turn 4 needs it: only what compaction kept can answer |

Each construct run gets a `compaction` scorecard (`bench/compaction.py`): cuts
by mode and reason, unreached cuts, summary fallbacks, provider overflows and
fatal ones, peak fill of the window, blocking ms, summarizer tokens and their
share, cache hit rate overall and right after a cut, and stub re-runs (a call
repeated after its result was stubbed: what a stub cost). `bench.py compare`
judges them like any other metric; `python3 bench/compaction.py RESULT.json`
prints one line per run.

```sh
python3 bench/compact_fixtures.py   # after changing the generator
python3 bench/bench.py task --model M --no-learn --rounds 5 \
  --tasks compact-long-fix compact-conventions compact-recall \
  --arm main@/path/to/main/construct: --arm new:
python3 bench/compaction.py bench/results/task-….json
python3 bench/bench.py compare bench/results/task-….json --a main --b new
```

## What the pilots changed

Two one-round pilots on `minimax/minimax-m3.1-flash:free` before the real
runs:

1. **compact-long-fix at a 20k window failed outright**: 31 cuts in 103
   requests, nothing fixed, money.py read eight times. Two causes. The
   summarizer's output cap (the summary allowance, 1750 tokens) was all spent
   on reasoning, so summaries came back empty or cut off (27 chars) and were
   accepted or fell back after ~15 s each (9 of 31 cuts). And when stubbing
   outside the last two rounds missed the target, the planner went straight to
   a summary and re-cut almost every round. Fixes: the summarizer gets 8192
   tokens of headroom for reasoning and a word budget, and a summary that
   ends on `length` or is empty is refused (its usage still counted); the
   planner escalates to stubbing everything but the newest round before
   summarizing. The suite moved to a 32k window: at 20k this model's habit of
   reading three whole files per round filled the target in two rounds.
2. **All three tasks at 32k passed** (`task-20261022-010543.json`): 4, 2 and
   10 cuts (3 of them summaries on compact-recall, whose turn-4 answer came
   back exact), no overflows, peak fill 0.61 / 0.60 / 0.82, cache hit 0.81–
   0.88 overall but 0.29–0.37 on the request after a cut.

## Baseline: main against the planner

`bench/results/task-20261022-053503.json`, 3 rounds × 3 tasks, numbers in
ADR 0013. `main` passed 6/9 (0/3 on compact-recall, every run ending in
fatal overflows); the planner passed 9/9 with no overflow and a 0.60 peak
fill. The price: where `main` squeezed through at 0.97 of the window it used
about half the tokens, and the planner's stubs get re-run (median 3 per run).
`keep_results = 4` matched it on success with lower medians on tokens,
requests and wall time, not significant at n=9.

## Steps

1. Planner, overflow recovery, journaled cuts, e2e. **Built.**
2. The suite, the window-enforcing proxy, `cfg:` and `@BIN` arms, the
   scorecard. **Built.**
3. Baseline `main` against the planner on the suite. **Done** (ADR 0013).
   Next: a sweep of `keep_results = 4` against a later cut (`threshold =
   0.85, target = 0.6`) and both, at 5 rounds, to settle the defaults.
4. After #51: warm summarizer and aging as cache-aware arms.

## Sources

- substrate_v3 (`compaction-features.md` in this repo's root is the merged
  feature list), and its finding that agents rarely recall from a journal.
- uniichat / OptMem (VictorTaelin): one long history, moderately aggressive
  pruning.
- opencode's durable project: a journal like substrate_v3's.
- Still to dig up: the paper on compaction windows and strategies, and how
  the DeepSeek harness compacts.
