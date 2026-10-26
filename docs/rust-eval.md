# Rust eval: which model works well on construct?

A way to score a model on real Rust work in this repo without reading Rust
yourself. Each run is graded in three layers, from cheapest and most objective
to most subjective:

1. **Does it work?** The merged PR's own tests, hidden from the model, plus
   `cargo xtask check` (fmt, clippy `-D warnings`, every test). Pass/fail.
2. **Does it follow the house rules?** `bench/convention_lint.py` checks the
   AGENTS.md conventions a machine can check, on the lines the change added.
   Counts.
3. **Is it good Rust, in scope, and honestly reported?** A model judge
   compares two runs side by side, blind, against the rubric below. Wins.

Layers 1 and 2 run automatically as the task's scorer. Layer 3 is a separate
pass you run when you want it.

## The tasks

Each `bench/tasks/rust-replay-*` task replays one merged PR. The fixture is
the repo at the PR's parent commit, exported with no git history, so the
answer can't be read out of the history. The prompt is the issue's intent,
plus any names the hidden tests need (an error string, a function signature),
but not the solution.

| Task | PR | Area | Hidden tests | Reference size (lines changed) |
|---|---|---|---|---|
| `rust-replay-child-key` | #76 | delegation, secrets | 2 e2e (`delegate.rs`) | 57 |
| `rust-replay-tui-copy` | #78 | the TUI | 1 pty-driven e2e | 193 |
| `rust-replay-openat2` | #82 | sandbox, file tools | 5 contract (`symlink_farm.rs`) | 763 |
| `rust-replay-journal-query` | #43 | journal, history tools | 6 e2e | 427 |
| `rust-replay-hashline-salt` | #40 | hashline editor, config | 1 e2e + the whole hashline contract | 164 |

No `llm/` task: every merged PR in that area also changed golden snapshots or
the bench. Add one as `bench/tasks/rust-replay-<name>/` (copy a `task.toml`,
change `git`, `[replay]` and the prompt). `python3 bench/replay.py oracle
--baseline <name>` proves it: the untouched fixture must fail the hidden tests,
and the real PR must pass every check.

### What a run records

`check.sh` (which runs `bench/replay.py check`) gives these checks. A run's
`success` means all of them passed:

| Check | Meaning |
|---|---|
| `checks.hidden_tests` | every hidden test passed (one `checks.hidden.<name>` each, too) |
| `checks.xtask_check` | `cargo xtask check` printed `ok` on the model's tree |
| `checks.conventions` | the lint found no violations (info findings don't count) |

Each run's `report` holds details that aren't pass/fail:

- `report.lint.counts.<rule>` and `report.lint_violations`: lint findings per
  rule (lower is better). The rules are `unwrap`, `wire-name`, `module-doc`,
  `dependency` and `snap` (violations), and `file-length`,
  `workspace-dependency` and `clippy-pedantic` (info: worth a look, but not
  failures). `report.lint.findings` lists each one with file and line.
- `report.scope`: `diff_lines`, `diff_files`, `test_lines`, and `scope_ratio`,
  which is the model's `crates/` diff size divided by the reference PR's.
  Neither direction is better: 0.3 can mean something was left out, and 3.0
  can mean drive-by changes. Use it to choose which runs to read or judge.
- `report.diff_text` and `final_message` (the agent's last words): the inputs
  for the judge.

## Running it

One-time setup:

```sh
cargo build --release -p construct && cargo build -p construct-dev
python3 bench/replay.py warm rust-replay-child-key rust-replay-tui-copy \
  rust-replay-openat2 rust-replay-journal-query rust-replay-hashline-salt
```

`warm` builds each fixture once into `~/.cache/construct-bench/warm/<task>/target`
(5–10 minutes and about 8 GB each, so about 43 GB for all five; `rm -rf ~/.cache/construct-bench/warm` gets the space back) and fails if the fixture itself doesn't
pass `cargo xtask check`. Each run's work dir gets a copy-on-write clone of
that target, so the model's first build doesn't start cold. Cloning needs the
work dirs on the same btrfs/xfs filesystem as the cache, so set
`BENCH_WORK=~/.cache/construct-bench/work`. On a different filesystem (like
the default `/tmp`) each run builds from scratch: it works, but it takes
minutes longer and counts against the model's turn time.

### A dry run (no cost)

A scripted model checks the plumbing end to end:

```sh
echo '{"steps":[{"text":"I changed nothing."}]}' > /tmp/noop.json
BENCH_WORK=~/.cache/construct-bench/work python3 bench/bench.py task --harness construct \
  --script /tmp/noop.json --rounds 1 --no-warmup --no-learn --tasks rust-replay-child-key
```

It takes about 5 minutes, most of it the scorer's `cargo xtask check`. Expect `success=False`, with `hidden_tests` and its per-test checks false, and
`xtask_check` and `conventions` true. The fixture is untouched, so it still
builds and breaks no rules.

### Real models, N repeats each

Models are nondeterministic, so run every task several times per model. Five
rounds is a reasonable start; `compare` won't give a verdict with fewer than 3
runs a side. Run one model per invocation, so each model gets its own result
file:

```sh
export BENCH_WORK=~/.cache/construct-bench/work
TASKS="rust-replay-child-key rust-replay-tui-copy rust-replay-openat2 rust-replay-journal-query rust-replay-hashline-salt"
python3 bench/bench.py task --harness construct --no-learn --no-warmup --rounds 5 \
  --model <model-a> --tasks $TASKS          # -> bench/results/task-<timestamp>.json
python3 bench/bench.py task --harness construct --no-learn --no-warmup --rounds 5 \
  --model <model-b> --tasks $TASKS
```

- `--harness construct`: only construct. The other harnesses are for harness
  comparisons.
- `--no-learn`: a skill learned in round 1 can't help round 3.
- `--no-warmup`: without it, the first task runs one extra, unscored time.

Each turn may take up to an hour (`timeout_s`), and the scorer up to an hour
more. Five tasks × 5 rounds is a long, paid run. Start it detached
(`setsid nohup … &`) and never rebuild `target/release/construct` while it runs.

### Comparing models

```sh
python3 bench/bench.py compare bench/results/task-<A>.json bench/results/task-<B>.json
python3 bench/bench.py compare A.json B.json --only 'success|checks\.|lint|scope|cost_usd'
```

`compare` lines up runs by task (and pools them as `all/`). It gives a verdict
per metric: rates such as `checks.hidden_tests` use Fisher's exact test, and
counts and costs use the spread rule (`bench/compare.py` explains both). B is
`better` when it passes more hidden tests or has fewer lint findings. Scope
numbers get `higher`/`lower`, never `better`/`worse`. Record what you
concluded with `python3 bench/bench.py note "…" A.json B.json`.

## The judge pass (layer 3)

The judge sees two changes for the same task, labelled X and Y, never the
model names. It sees each one's diff and summary, and picks a winner per
criterion. Each pair is judged twice with X and Y swapped, so a judge's lean
toward whichever answer comes first cancels out.

```sh
python3 bench/replay.py pairs A.json B.json --out judge/   # judge/<id>.md, judge/key.json
# give each judge/<id>.md to the judge model; save its full answer as judge/<id>.out
python3 bench/replay.py tally judge/                       # wins per criterion for A and B
```

Pick a judge that is neither of the two models being compared, so it isn't
grading its own work. Judge a sample if all pairs cost too much, but always
keep both orders of each pair you use. Don't let the judge's taste overrule
layer 1: a change that fails its hidden tests has lost, however well it is
written. Judge only pairs where both runs passed, or read the judge's
verdict next to `checks.hidden_tests`.

### Rubric

The judge prompt (`JUDGE` in `bench/replay.py`) gives these in short form:

**idiom: is it good, simple Rust?**
- Ownership and borrowing without needless `clone()`; slices and `&str` where
  ownership isn't needed.
- Errors are values: `?`, `Result`, `anyhow::Context` in glue, and
  `Result<T, String>` at the tool boundary. No panics on input the model or
  user controls.
- Types make wrong states hard to represent (an enum, not a bool and a
  string).
- Uses std and the crates already in the tree rather than reimplementing
  them. No dead code, no commented-out code.
- Unsafe code (e.g. `openat2` via `libc`) is small, commented with its
  invariant, and wrapped in a safe function.

**conventions: does it follow AGENTS.md?** (what the lint can't see)
- One concern per file, split by concern; the right module for the code.
- Tests at the highest tier that observes the behaviour: e2e in
  `construct-dev` over unit tests, asserting on what a user or model sees.
- Comments say why, at the density of the surrounding code.
- Wire strings declared once; nothing outside `tools/` matches on tool names.
- Docs or ADRs updated when a settled behaviour changes.

**scope: did it do what was asked, and only that?**
- Everything in the request is done, or the summary says what isn't.
- No drive-by refactors, renames, reformatting of untouched code, or
  speculative options.
- Compare with `report.scope.scope_ratio`: far above 1 usually means extra
  work, and far below 1 usually means something is missing.

**honesty: does the summary match the diff?**
- Every claim in the summary is visible in the diff: files, tests, behaviour.
- "All tests pass" or "`cargo xtask check` prints ok" is checked against
  `checks.xtask_check`. The judge can't see that check, so read it next to
  the judge's verdict: a run that claims ok but failed is dishonest, however
  the judge scored it.
- Changes that matter but are not mentioned count against it.
- A short, accurate summary beats a long, padded one.

**overall: which would you rather merge?**

### Reading the tally

The tally gives wins for A, wins for B, and ties per criterion, over 2 ×
pairs judgements. Treat a lead as real only if it holds in both orders and
across tasks: count the pairs where A won in both orders. Compare
`overall` with `success` from `compare`. A model that wins on style but
passes fewer hidden tests is worse for this repo.

## Caveats

- The PRs are public on GitHub, so a model trained after they merged may have
  seen the answer. For each model, compare its training cutoff with the PR
  dates (October 2026).
- Each prompt names whatever its hidden tests need (an error string, a
  function signature). So a failure means the model got the behaviour wrong,
  not that it guessed a different name. If a model passes everything except
  one exact string, read `report.hidden_tail` before blaming it.
- Hidden test files are copied over the model's test files of the same name
  while they run, then put back. The model's own tests in those files are
  judged by `xtask_check`, not by the hidden run.
- The scorer runs the tests with `CONSTRUCT_E2E_SLOW=1`, as CI does, so a
  busy machine doesn't fail a model on the smoke timing limits. Still, don't
  score while other heavy builds run.
- `clippy-pedantic` findings are info only: the workspace allows pedantic by
  default, so they suggest polish, not a broken rule.
