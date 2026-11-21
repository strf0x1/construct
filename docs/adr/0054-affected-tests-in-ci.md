# 0054 — Pull requests run the tests their diff reaches

Status: accepted. 2026-11-20.

## Context

CI ran every test on every pull request. The Linux `check` job took
4–5 min and the macOS job 5–7 min. On macOS the end-to-end suite (`it`,
273 tests) was 136 s of the 229 s test step. Most PRs touch one area,
yet every PR paid for all of it: the bench's Python tests (67 s on CI)
ran for Rust-only changes, and `crash_sweep` (the slowest file, ~20 s
locally) ran for TUI changes that `run --json` never reaches.

We looked at deriving reach from the code instead of declaring it. It
doesn't narrow anything here:

- **Crate graph.** Every e2e test runs the real binary, which links every
  crate, so any change under `crates/` reaches all of `it`.
- **Module graph** (`docs/architecture.toml`). `config` names every
  subsystem's settings (`jobs`, `mcp`, `tools`, `learn`, …), and almost
  every module names `config`. The reverse closure of any leaf module is
  the whole crate.
- **Per-test coverage** (`cargo-llvm-cov`, child-process profiles). This
  would be exact, but it is a pipeline of its own to keep a 273-test suite
  fresh.

## Decision

`cargo xtask check --affected`, which CI runs on a pull request:

1. **Crate tests.** Run the unit and contract tests of every crate the diff
   touches, plus every workspace crate that depends on one, transitively,
   read from the manifests. `--quick` uses the same reach.
2. **The bench's Python tests.** Run them only when `bench/` or a
   workspace file changed.
3. **End-to-end files.** Run only the files of `it` that the diff reaches,
   as libtest filters (`-- jobs:: e2e::`). Reach comes from
   `crates/construct-dev/tests/affected.toml`: areas of source paths, each
   naming the test files that exercise it.
4. **Unlisted means everything.** A changed path under `crates/` that no
   area names runs the whole suite. So do the workspace files (`Cargo.*`,
   `.cargo/`, `.github/`, `clippy.toml`) and `tests/common/`. Only an area
   can narrow. A test file reaches itself, and `tests/snapshots/` reaches
   `snapshots`. Other paths outside `crates/` reach nothing unless an area
   names them, because the tests read the repo only through
   `common::root()`.
5. **Full run on main.** Every push to main runs `cargo xtask check` in
   full, on Linux and macOS. That run is the net under an area that claims
   too little.

The `affected` gate checks that every path in the table exists and every
test it names is a module of `tests/it.rs`. It cannot check that an area
is complete.

## Consequences

- PRs confined to the TUI, ACP, Deck tools, delegation, jobs, MCP, web,
  history, or to the repo's tooling, docs or bench, run a
  fraction of `it`.
- Changes to the spine (the agent loop, llm, event, journal, config,
  prompt, compaction, router, memory, skills, learn, sandbox, permission
  checks, providers, models, secrets, serve) still run everything. That is
  correct, not a gap in the table: every test runs through them.
- An area that misses a test file lets a PR merge green and break main.
  We fix forward, then add the file to the area. If that happens often,
  the answer is a merge queue that runs the full check, not more areas.
- A new e2e file is selected by its own path. If it exercises an area, it
  must be added to that area's `tests`, or that area's changes won't run
  it until main.
- Revisit if the suite grows enough that declared areas drift. Measured
  per-test coverage is then worth its pipeline.

## Evidence

- CI timings before (run 44712844797, main): Linux `check` 282 s
  (test 129 s, bench 67 s); macOS 304 s (test step 229 s, of which `it`
  136 s).
- Per-file `it` timings, 2 threads, local: total ~100 s; `crash_sweep`
  20 s, `e2e` ~10 s, `developer` 10 s, `permission` 10 s, `jobs` 10 s,
  everything else ≤5 s.
- The Deck-tools area's filters, run locally: 106 of 273 tests, 27 s.
- `xtask::affected` tests pin the repo table: a TUI or ACP change leaves
  out `crash_sweep`, a jobs change keeps it, and `agent.rs`, `config.rs`
  and `cli.rs` reach everything.
