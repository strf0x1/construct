# 0024 — The search tool stays `grep`, on ripgrep's crates; fff is not adopted

Status: accepted, 2026-10-29.

## Context

Issue #10 asked for the fff crate (`fff-search`) as the search tool, to be
benched for speed, results and tool failure rate on at least three models. Two
hypotheses came with it: that fff is faster in a way that matters to an agent,
and that a search tool not called `grep` would be used less, because models
reach for the shell when they do not see the name they know.

Two things were measured, in this order: the engines (no model), then the
tool as models use it.

Findings that shaped the second part:

- The grep tool of the time walked and searched on one thread, in directory
  order. "fff vs grep" would have credited fff with what parallelism alone
  gives, so the baseline for any engine claim is the same crates run in
  parallel (PR #104).
- fff's grep takes one query string with constraints inside it (`*.rs`,
  `!test/`, and with AI mode a token like `self.tx` becomes a file-path
  constraint). A regex tool built on it must build the query itself.

## Decision

1. **The content-search tool is named `grep` and runs ripgrep's crates**
   (`grep-regex`, `grep-searcher`, `ignore`), walking and searching in
   parallel and listing matches in path order, capped at 200 lines with the
   total counted (`tools/grep.rs`, PR #104).
2. **`fff-search` is not a dependency.** Anyone proposing it again must show
   a win over the parallel ripgrep engine on a bench with tasks that can
   fail, and answer the costs below.
3. **`[tools] search_name` stays, default `"grep"`, as an experiment knob.**
   It changes only the wire name. The system prompt names the search tool as
   the session has it, or not at all when `grep` is disabled
   (`ToolsConfig::search_tool`, `prompt::base`); a prompt must never name a tool
   the model does not have.
4. **The search suite stays** (`bench/tasks/search-*`, built by
   `bench/search_fixtures.py` and `search_fixtures_hard.py`; scored by
   `bench/search_report.py`), as the way to ask whether a change to the search
   tool matters to a model.

## Consequences

- No new dependency, no index, no watcher. Peak RSS of a scripted run is
  15-17 MB on repos up to a few thousand files and 25 MB on the linux tree
  (the parallel walk's per-worker buffers); fff added +27 MB on a 3k-file
  repo, +165 MB on linux, +350 MB with its content index.
- Search latency on a large repo drops without fff: the linux tree (97k
  files) went from 1025 / 473 ms to 122 / 69 ms per call.
- **What this does not settle:** whether the search tool earns its place at all.
  On these tasks a model with no search tool did as well (below). The tool's
  other properties (honours `.gitignore`, reads through the sandbox's
  `openat2` path, bounded output) are not what this bench measures, and are
  the reason to keep it.
- Revisit when: a bench whose tasks can fail (longer sessions, repos large
  enough that shell search is slow, a ranked-results arm) shows a difference
  between having the tool and not; or a stable `fff-search` fixes the
  regex line-number bug and offers a freshness guarantee after a write.

## Evidence

**Engines** (`bench/search-engines/`, a standalone crate; README has the
tables; raw output in `results/`). fff-search 0.11.0, 24 cores, warm cache.

- Speed on linux (97k files), median ms, seq (the tool then) / parallel / fff:
  rare literal 528 / 71 / 21; no match (full scan) 525 / 71 / 44. With fff's
  content index: 3.2 and 10.5. On cargo (2.8k files) every engine but `seq`
  answers in under 10 ms (parallel 5, fff 0.3).
- Fidelity: in regex mode `^\s*impl\b` returned the blank line *before* each
  match (`\s` crossed `\n`): +425 of 433 lines on ripgrep, +1847 on tokio,
  +939 on cargo, +484 on linux, against the rg binary. Fixed in
  `0.11.1-nightly.d1c7dd4`, not in a stable release. Hidden files are searched
  (the tool skips them); files with NUL bytes are skipped (rg searches them up
  to the NUL).
- Freshness: with the watcher off the index never saw a written file; with it
  on, ~75-80 ms, and a grep right after a write missed.
- Build: 116 crates, vendored libgit2 and LMDB, ~75 s extra cold build.

**The tool, as models use it.** Branch `search-naming-bench`; the construct
binary was built from `d501de0` plus the `search_name` change later committed as
`4ea4ac4` (so the manifests read "dirty"; every arm's tool list in its manifest
shows `grep`, `search` or neither as intended). Bench `d256af8`.

- Arms: `grep` (the tool, named `grep`), `search` (same schema and description,
  named `search`), `none` (`disabled = ["grep"]`). Eleven tasks: six small
  (`search-definition`, `-callers`, `-config`, `-rename`, `-count`,
  `-signature`; ~90 files) and five on ~1,500-file repos built per run
  (`search-hard-trace`, `-unused`, `-env`, `-rename`, `-errorcode`). Answers are
  computed from the generated tree; `bench/test_search_tasks.py` checks each
  scorer accepts the right answer and rejects the believable wrong ones.
- Models: `anthropic/claude-haiku-5.5`, `deepseek/deepseek-v4.1-flash-1017`,
  `openai/gpt-6-luna`. 10 rounds, 110 runs per arm per model, 990 runs, none
  over budget or broken by the provider. Cost $1.65.
- Result files: `bench/results/task-20261028-221453.json` through
  `task-20261028-234020.json` (15 files, five 2-round batches per model);
  report `bench/results/search-report-20261029.md`. Pilots (1 round, 18 to 33
  runs a model): `task-20261028-124153.json`, `-155248.json`, `-160023.json`,
  `-160141.json`.

Adoption (of file-content searches, the share made with the tool and not a
shell command), with a 95% bootstrap interval for grep minus search:

| model | `grep` | `search` | difference |
|---|---|---|---|
| Haiku 5.5 | 0.57 | 0.62 | -0.05 [-0.17, +0.07] |
| DeepSeek V4.1 Flash | 0.37 | 0.25 | **+0.12 [+0.04, +0.20]** |
| GPT-6 Luna | 0.84 | 0.82 | +0.03 [-0.07, +0.12] |

The one interval that excludes zero is DeepSeek's, which went the other way in
the 11-run pilot (0.27 `grep`, 0.47 `search`); its "used the tool at least
once" rate (0.80 vs 0.69) does not exclude zero (+0.11 [-0.002, +0.225]);
`bench.py compare` judges `all/search.adoption` `same`; and about 50 intervals
were computed, so a few false positives are expected. Taken together: no
consistent effect of the name. Adoption varies far more by task (Haiku and
Luna: 0.9-1.0 on definition, error-code and call-chain lookups; ~0 on counting
and config) and by model (DeepSeek rarely uses the tool).

Tool versus no tool (`grep` / `search` / `none`):

| | success of 110 | requests/run | $/run |
|---|---|---|---|
| Haiku 5.5 | 107 / 108 / 108 | 4.23 / 4.34 / 4.19 | .0015 / .0015 / .0014 |
| DeepSeek V4.1 Flash | 110 / 110 / 107 | 6.53 / 6.71 / 7.04 | .0019 / .0029 / .0024 |
| GPT-6 Luna | 108 / 106 / 106 | 5.38 / 5.25 / 5.39 | .0010 / .0011 / .0012 |

No success difference (Fisher p >= 0.25). Differences that went against "the
tool changes nothing", none of which `bench.py compare` (medians and spreads)
confirms: Luna used 3.7k fewer prompt tokens a run with `grep` than with no tool
(interval [-7.0k, -0.5k]); Haiku's tool failures were 2.2% of calls with no
tool against 0.2% with `grep` (7% of runs against 1%, p = 0.04). What those
failures were was not inspected; a shell `grep` that matches nothing exits 1, so
some may be that.

**Limits.** Cheap models; single-turn tasks; success is 96-100% in every arm, so
success cannot separate them; the parallel-grep build ran in every arm; the
shell-search classifier (`bench/process.py`) is a regex over the command, tested
(`bench/test_search_tasks.py`) but a heuristic; result ranking was not benched.
