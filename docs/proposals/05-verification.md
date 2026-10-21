# Proposal 5 — Verification an agent can run

Status: plan. Written 2026-10-20 against `1e83f33`.

Most of construct is written by agents, so the question "is this change good"
has to be answerable by an agent, deterministically where possible, and
without reading transcripts. construct already has the hard parts: a scripted
model endpoint that records every request, a pty driver that snapshots the
screen, e2e tests on the real binary, and a bench that scores real-model runs
with scripted checks. What is missing is the layer that turns those into
*verdicts*: one command, one exit code, one diff to read when it fails.

## Where it stands today

| Need | Have | Gap |
|---|---|---|
| "Did I break anything" | `cargo test` (4 unit), `cargo test -p construct-dev` (10 e2e, 11s) | two commands, no fmt/clippy gate, no single verdict |
| "Did the request to the model change" | `Env::requests()` + hand-written asserts per test | no golden snapshot; the first-request-size rule is asserted as `bytes < 6000` in one test |
| "Did the screen change" | `drive snap` prints the screen | nothing compares it to anything |
| "Is it faster/smaller" | `bench.py size\|frame\|tools\|memory`, `report.py` renders the latest | no baseline-vs-candidate comparison, no noise bound, no verdict |
| "Does it do the task" | `bench.py task\|routing\|loop` with `check_*()` scripted checks | tasks are Python functions inside bench.py; adding one means editing a 1200-line file |
| "What produced this number" | shas typed by hand in observations.md | no manifest in result files |
| "How did the agent behave" | session JSONL, endpoint records | metrics need transcript reading; tool results are prose |

## Three tiers, one table in AGENTS.md

Label every check with what it costs and whether it is deterministic, and say
which changes require which tier. Agents are good at following a table.

| Tier | Command | Deterministic | Cost | Required for |
|---|---|---|---|---|
| 0 check | `cargo xtask check` | yes | ~1 min warm, no network | every PR |
| 1 overhead | `bench.py size frame tools memory` (mock model) | timing noise only | ~5 min, no network | anything touching startup, the TUI loop, tool execution, request building |
| 2 behaviour | `bench.py task\|routing\|loop\|edit --model …` (real model) | no | minutes to an hour, costs money | prompts, tool schemas or wording, router, learning, editor |

Tier 0 is the one that matters most and the cheapest to finish. Tier 1 and 2
exist already; the work is making their output a verdict.

## Tier 0: `cargo xtask check`

Runs, in order, stopping at the first failure:

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (unit, contract, e2e)
4. golden snapshots (below) as part of 3

Prints one line per gate and ends with `ok` or `FAILED <gate>`. Nothing else.
An agent that sees `ok` is done; one that sees `FAILED snapshots` knows to run
`cargo insta review` (or `cargo xtask snapshots` to accept) and read the diff.

### Golden request snapshots

The single most valuable deterministic check, because it protects the two
things that are easiest to regress without noticing: the system prompt and
the tool definitions. Both go to the model on every turn and both are
"keep the first request small" in the rules of thumb.

Mechanism: one e2e scenario per shape of first request (plain turn; turn
with a routed skill; turn with web routed; MCP routed; learning review
request), each with a fixed `CONSTRUCT_HOME` fixture. After the run, take
`Env::requests()[0]`, normalise the fields that legitimately vary (cwd, date,
session id, the `os:` line), and compare to a committed snapshot with
[`insta`](https://insta.rs) (`assert_json_snapshot!`). `insta` is the standard
Rust answer here: snapshots live beside the test as `.snap` files, a mismatch
prints a readable diff, and `cargo insta accept` records an intended change.

What that buys:

- A refactor PR (proposals 03 and 04) is *proven* behaviour-neutral by an
  unchanged snapshot, instead of by the author's claim.
- A prompt or schema edit shows up as a diff in the PR, so a reviewer sees
  exactly what the model will see differently.
- Request size is asserted as a number in the snapshot's metadata
  (`bytes`, `n_tools`), with the limit next to it.

Do the same for screens: `drive snap` already produces the vt100 screen as
text. Snapshot the screen after the first frame and after a finished turn with
a table, at a fixed size. Theme or layout changes become reviewable diffs
rather than "looks fine".

Rule for agents: **never edit a `.snap` file by hand.** Run the tests,
look at the diff, and if the change is intended accept it with the tool and
say so in the PR description.

### Contract tests

Proposal 04 rule 5: each tool has a model-free test of its promise. These live
in `crates/construct/tests/` and run in tier 0. They are where the hashline
port's 64 ported tests go, and where "skill patching is unaffected by
`editor`" is pinned.

### Timing in tests

Pass/fail tests must not depend on the machine. The e2e `first_frame_ms <
500` assertion and the `wait … 3000` budgets stay as smoke limits, doubled
under `CONSTRUCT_E2E_SLOW=1` (set in CI). The real first-frame number is the
`frame` bench's job.

## Tier 1 and 2: results that are verdicts

### Provenance in every result file

`bench.py` `save()` adds a `manifest` block to every result:

```json
"manifest": {
  "construct": { "git_sha": "…", "dirty": false, "binary_sha256": "…", "editor": "replace", "router": "local" },
  "dev_git_sha": "…", "model": "…", "bench_py_sha256": "…", "host": "…", "started": "2026-10-20T21:18:06Z"
}
```

The construct part comes from `construct manifest` (proposal 04). A result
without a manifest is not citable in observations.md.

### `bench.py compare`

```
python3 bench/bench.py compare results/tools-A.json results/tools-B.json
```

For every metric both files share: median delta, the spread of each side
(p10–p90), and a verdict per metric: `better`, `worse`, `same` (delta inside
the larger spread), or `inconclusive` (n too small). Exit 0 for no `worse`,
1 otherwise. Print one table; write the same as JSON next to the results.
This replaces reading two files side by side, which is what an agent does
today and what humans get wrong.

For real-model suites the rule is the one already in the README: post the
number either way. `compare` makes the number; the agent appends the table and
the two result paths to `observations.md`. A short `bench.py note "<text>"`
that appends a dated entry with the manifest and the result paths keeps that
log consistent.

### Tasks as directories

Today a behavioural task is three Python functions (`*_fixture`, `check_*`,
`suite_*`) inside bench.py. Move each task into a directory the bench loads:

```
bench/tasks/stats-median/
├── task.toml        turns = ["…", "…"]; timeout_s; budget = { max_requests = 40, max_prompt_tokens = 200000 }
├── fixture/         copied into the work dir, `git init`ed
└── check.sh         run in the work dir after the last turn; prints one JSON object of named booleans
```

`check.sh` is the scorer and the whole definition of done. It never reads the
transcript. It may run the project's tests, probe behaviour, grep for a README
heading, and diff `git status` against the paths the task expects to change.
`bench.py task --tasks stats-median,…` runs any set; the three existing
tasks (task, routing, loop) become directories with no change to what they
measure. Issue #15's realistic projects and the editor corpus in proposal 04
are then just more directories, which is what makes adding them a
mid-tier-model job.

Budgets in `task.toml` turn runaway runs into a scored failure (`over_budget`)
instead of a 900s timeout the agent has to interpret.

### Process metrics without reading logs

For multi-step runs the question "how did it get there" matters as much as
"did it get there", and today that means reading a session file. With
`Event::ToolEnd.data` and `ToolError.code` (proposal 04) the session log
carries structured results, and `bench.py` can compute per run, mechanically:

- tool calls by name; errors by `code`; retries (same tool, same primary arg,
  within two steps of an error); re-reads after a failed edit,
- edit rejections and recoveries (for the editor experiment),
- prompt tokens per step and the step at which they exceeded the first
  request by 10x (context growth),
- routed groups per turn and whether each routed group was then used
  (routing precision), which the `routing` suite computes today from
  `session_facts()`.

These go into the result file beside `checks`, so `compare` can compare them.
An agent reads a transcript only when a number is surprising.

### Arms

`bench.py task --arm replace:'editor="replace"' --arm hashline:'editor="hashline"'`
writes each arm's `[tools]` overlay into its own home's `config.toml`
(`Harness.configure` already writes that file), interleaves rounds, records
the arm in the manifest, and `compare` works across arms in one file. This is
the "orchestrator-facing way" in issue #1's criteria, and it is twenty lines
on top of what exists.

## What this gives an agent landing a feature

Before: build, run two test commands, maybe run a bench, open two JSON files,
decide. After:

1. `cargo xtask check` → `ok`, or a diff that says what changed in what the
   model sees.
2. If the change touches a tier-1 or tier-2 area (the table says which):
   `bench.py <suite>` on main and on the branch, `bench.py compare a b` →
   verdict table, exit code.
3. `bench.py note "…"` to record it. Open the PR with the `check` line and the
   `compare` table pasted in.

The agent never has to interpret cargo output, never diffs result files by
eye, and never reads a transcript unless a number sends it there.

## Order of work

1. `cargo xtask check` with the four gates and the one-line verdict. (Small;
   do it with proposal 03 phase 0.)
2. `insta` request snapshots for the five first-request shapes, and two screen
   snapshots. Land before any refactor PR, so the refactors have an oracle.
3. `manifest` in result files; `bench.py compare`; `bench.py note`.
4. Tasks as directories; move the three existing tasks; add budgets.
5. Structured tool results (with proposal 04 step 1) and the process metrics.
6. `--arm`.

Steps 1 to 4 need no change to construct itself and can proceed in parallel
with proposals 03 and 04.
