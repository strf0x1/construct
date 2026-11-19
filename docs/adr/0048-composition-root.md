# 0048 — composition root: construct binary + construct-core lib

Status: accepted. 2026-11-19.

## Context

Crate modularization (#274) step 2 (#276): split `crates/construct` so the
CLI leaves the library before `construct-tui` (#277) is extracted. `cli`
starts `tui` and `tui` uses the agent core, so extracting the TUI while
`cli` stays in the library would be a crate cycle. The composition root
moves first.

Two shapes were on the table:

- **Option A** — keep the engine package named `construct`, move the binary
  into a new thin crate. Every `construct::` import keeps working, but every
  site that builds or names the binary package changes: `cargo build -p
  construct`, `.github/workflows/release.yml` (build and `cargo-about`
  license generation), `crates/construct-dev/tests/common/mod.rs`,
  `scripts/try.py`, the verify-construct doctor/drive scripts, `bench/bench.py`,
  `bench/restore_cost.py`, README — and the shipped crate names end up
  mismatched with the binary they produce.
- **Option B** — the binary keeps the package name `construct`; the engine
  becomes `construct-core`. All of the above stay unchanged; the one-time
  cost is mechanical `construct::` → `construct_core::` in the test files.

## Decision

Option B. Two workspace packages:

- `crates/construct` — the composition root. `[[bin]] name = "construct"`
  (`test = false`), no `[lib]`. Owns `main.rs`, `cli.rs`, `cli/` and `clap`.
  `main.rs` declares `mod cli;` and re-execs the sandbox helper; everything
  testable lives in the engine.
- `crates/construct-core` — the engine. `[lib]` only (`doctest = false`),
  `autotests = false` with the one `tests/it.rs` binary and `tests/fixtures/`.
  Owns every non-CLI module, including `tui` until #277 extracts it.

`construct-core` is pinned in `[workspace.dependencies]` and taken with
`.workspace = true` by both consumers (the binary and `construct-dev`'s
dev-dependency), per convention 8. `build.rs` moves with the engine: it
bakes `CONSTRUCT_GIT_SHA`/`CONSTRUCT_GIT_DIRTY` for `manifest.rs`, and its
dirty check watches both shipped crates' sources.

The move is one pure `git mv` commit (no edits mixed in), recorded in
`.git-blame-ignore-revs`; rewiring, manifests and tooling follow in separate
commits. `CONTRACT.lock` changes only its `vendored` path — version, commit
and `sha256` are untouched, so no ACP wire change.

## Consequences

- `cargo build -p construct`, `cargo build --release -p construct`, the
  release workflow, `cargo-about --manifest-path crates/construct/Cargo.toml`
  (transitive licenses included), the bench and verify scripts, and README
  all work unchanged: `target/{debug,release}/construct` is still the binary.
- Unit and contract tests are now `cargo test -p construct-core`; `clap` is
  not a direct dependency of `construct-core` (it remains in the transitive
  tree via `secretspec`, whose clap dependency is non-optional upstream —
  true before the split as well). The binary crate has no tests.
- The first real cross-crate edge exists: `construct` → `construct-core`,
  listed in `[crates.construct] may_use_crates` (ADR 0047's machinery, proven
  by gate tests until now). `cli`'s engine references are crate edges; its
  `may_use` is empty.
- `tui` stays in `construct-core` for now with
  `used_only_by = { modules = [] }`: nothing inside the engine may reach it;
  only the binary starts it. #277 moves it to `construct-tui` and turns the
  module rule into crate-level rules.
- `cargo xtask check --quick` discovers workspace crates under `crates/*`
  instead of hardcoding two: a changed crate runs its tests, a binary-only
  crate with `test = false` is skipped, and a future `construct-tui` is
  picked up without a tooling change.
- One visibility change: `providers::Provider::check` becomes `pub` (it was
  `pub(crate)`; the CLI's `providers add` validates a declaration with it).
  The issue's "zero `pub(crate)` items called by cli" pre-check missed this
  one; it is the only such item.
- Path-keyed tooling follows the move in the same PR: ratchet keys,
  feature-map `depends:` paths, the feature-map gate's `help.rs`/theme paths,
  `bench/convention_lint.py`'s `LONG_OK`/`TOOLS_DIR`, and AGENTS.md's layout
  table. The 12 `verified` feature-map entries are re-driven after the move
  and their `verified_at`/`verified_deps` bumped in a follow-up commit.

## Evidence

- #276 (this change), part of #274; step 1 was #275 / PR #280 / ADR 0047.
- Pure-move commit `caaefa25c553f7be3c253dbaa07b6e8c8629aead` (281 files,
  0 insertions, 0 deletions), recorded in `.git-blame-ignore-revs`.
- `cargo xtask check`: docs, map, contract, arch, fmt, clippy and workspace
  tests green; golden snapshots and e2e unchanged. (Local bench gate: four
  pre-existing macOS environment failures, reproduced on clean `origin/main`
  — BSD `sed -i` and `/var/folders` TMPDIR symlinks; CI runs Linux.)
- `scripts/acp-contract.sh verify` passes offline; `CONTRACT.lock` diff is
  the `vendored` path only.
- `cargo build --release -p construct` produces `target/release/construct`;
  scripted-model smoke (`scripts/mock.sh` + `construct run --json`) works.
- The 12 `verified` recipes re-driven via `drive.sh` after the move; run ids
  and PASS results recorded in the PR.
