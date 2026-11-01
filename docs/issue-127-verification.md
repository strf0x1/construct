# Issue 127 verification

Implemented on `codex/127-session-browser` in the isolated worktree
`/home/brandon/git/construct-worktrees/issue-127`.

Implementation: `29be365`. Recipe updates: `549797b` (expanded help height)
and `3cfd30f` (wait for the complete palette frame). Verification date:
2026-10-31, America/Denver.

## Tier 0: deterministic behavior

- `cargo xtask check --quick`: PASS (docs, feature map, architecture and line
  ratchets, formatting, clippy, construct unit/contract tests, construct-dev
  unit tests and the benchmark machinery's own tests).
- `cargo test -p construct-dev --test it -- --test-threads=4`: **163 passed,
  0 failed, 1 ignored**. This includes all existing end-to-end scenarios and
  request/UI snapshots, plus session discovery, switching, permissions,
  compaction, crash recovery, ownership and write-failure scenarios.
- `cargo xtask snapshots`: only the help-panel snapshot changed, intentionally
  adding `/resume` and `/rename`. Existing model-request snapshots pass.
- Feature-map structural validation: PASS.

The new scenarios cover launch cancellation without credentials or an empty
journal, complete archive searching beyond a page, Unicode/narrow terminals,
manual names and recency, normal restart, A → B → A context/model isolation,
fresh permissions on every runtime, busy turns/queued prompts/jobs, directory
isolation, existing `-c`, CLI overrides, unavailable providers, unreadable
history, full historical UI alongside compacted context and degraded journal
writes.

Forced termination uses real processes: during routing, during streaming,
and after one tool has committed its outcome while a second has not. The
next request has complete tool-call/result pairing. Known results are
recovered, unknown effects are explained, effects are not repeated and a
second opening adds no repairs. A concurrent writer is refused while the
original process owns the session, then succeeds after termination.
Recovered job completions wait for an explicit user prompt and are delivered
once; jobs whose process was lost are marked interrupted.

## Tier 0: driven TUI

All recipes PASS. The images were opened and inspected. Their `check.sh`
scripts inspect journal rows, requests or tool outcomes independently of the
scripted model's fixed replies.

| Recipe | Run ID | Commit |
|---|---|---|
| session-browser | 20261031-issue-127-549797b | 549797b |
| slash-commands | 20261031-issue-127-549797b | 549797b |
| slash-palette | 20261031-issue-127-palette | 3cfd30f |
| model-palette | 20261031-issue-127-549797b | 549797b |
| models | 20261031-issue-127-549797b | 549797b |
| status-line | 20261031-issue-127-549797b | 549797b |
| scroll-copy | 20261031-issue-127-549797b | 549797b |
| background-job | 20261031-issue-127-549797b | 549797b |
| background-delegate | 20261031-issue-127-549797b | 549797b |
| delegate-progress | 20261031-issue-127-549797b | 549797b |

Evidence remains under `.verify/evidence/<run-id>/<recipe>/` in this worktree:
transcripts, full model requests, journal rows and PNGs. The new browser
recipe specifically proves two persistent IDs and names, original prompts,
per-session usage rows, the selected models, valid tool pairing and no
cross-session context in the next requests.

## Tier 1: optimized build comparison

Baseline `169938e` versus implementation `29be365`, built with the repository's
normal release profile. Every result contains binary/config manifests. Frame/tool/memory runs
use scripted endpoints, the local router and no learning. Size runs use the
default one-prompt setup. Frame runs have
five interleaved rounds, tools three rounds with 15 read/shell pairs, and
memory three rounds with 120 tool steps, plus warm-ups. The memory corpus is
OpenCode `977f5c695c2b4a254560743432d20f622cbe1233`, `packages/core`.

| Metric | Before | After | `bench.py compare` |
|---|---:|---:|---|
| First frame, median | 2.568 ms | 2.475 ms | same |
| First output, median | 1.977 ms | 1.912 ms | same |
| Ready, median | 2.641 ms | 2.542 ms | same |
| Read overhead, median | 0.547 ms | 0.575 ms | same |
| Shell overhead, median | 2.652 ms | 2.699 ms | same |
| Tool workload wall time, median | 77.065 ms | 75.232 ms | same |
| Median mean RSS | 36.283 MiB | 37.133 MiB | **worse: +0.850 MiB** |
| Median peak RSS | 43.852 MiB | 46.773 MiB | same (within measured spread) |
| Memory workload wall time | 5.764 s | 5.777 s | same |
| Memory workload requests | 121 | 121 | same |
| Memory workload exit code | 0 | 0 | same |
| Final request size | 246.245 KiB | 246.244 KiB | better (one-byte fixture-path difference) |
| Release binary | 21,702,152 B | 21,880,712 B | +178,560 B (+0.823%) |
| Fresh home after first run | 37,271 B | 37,271 B | unchanged |

The average-memory comparison exits 1 with a `worse` verdict. This is recorded
as the measured cost of the session features, rather than reported as a
uniform performance pass. The binary increase is below 1%; the memory
workload still completes with the same request count and no measured latency
regression. These costs are accepted for this change (ADR 0029). The
one-byte request difference comes from the differing arm-directory names,
not improved request construction.

Size has one deterministic measurement per binary; its paired comparison
correctly reports timing/size statistics as inconclusive with `n=1`. The
paired size artifact derives only from the two recorded measurements.

Raw results and comparison tables are in `bench/results/`:

- `size-20261031-163315.json`, `size-20261031-163729.json`, `size-issue-127-paired.json`
- `frame-20261031-163910.json`, `compare-20261031-163951.json`
- `tools-20261031-163911.json`, `compare-20261031-163951-1.json`
- `memory-20261031-163958.json`, `compare-20261031-164042.json`
- `compare-20261031-164010.json` (paired size)

All model traffic in this verification was scripted; no real-provider behavior benchmark was run.
