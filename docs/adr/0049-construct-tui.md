# 0049 — construct-tui: the first component crate

Status: accepted. 2026-11-19.

## Context

Crate modularization (#274) step 3 (#277), after the composition root
(#276, ADR 0048). `tui` was a sink inside `construct-core`: nothing in the
engine used it (`used_only_by = { modules = [] }` proved it), so extracting
it needs zero cycle-breaking. Every UI edit rebuilt the whole engine and
relinked its test binaries; the engine carried four UI-only dependencies
(`ratatui`, `crossterm`, `pulldown-cmark`, `unicode-width`).

Two seams needed a decision:

- `text.rs` mixed byte-budget cutting (`head`/`tail`, used across the
  engine) with screen-width arithmetic (`cols`, `cols_after`, used only by
  the TUI) and a `#[cfg(test)]` proptest generator used by both sides.
- The TUI's three invariants (`secrets`, `jev`, `sandbox` bans) were
  module-level `must_not_use` rules; cross-crate edges were crate-granular
  (ADR 0047), so the bans would silently disappear on extraction.

## Decision

- `crates/construct-tui` is the ratatui front end. `construct-core` drops
  the `tui` module and all four UI-only dependencies. Only `construct`
  depends on `construct-tui` (`used_only_by = { crates = ["construct"] }`);
  the cli calls `construct_tui::run(...)`. The engine runs without a
  terminal (`construct run --json`, children, the learner) unchanged.
- The three bans become module-scoped crate denies
  (`must_not_use_crates = [{ crate = "construct-core", module = "…",
  reason = "…" }]`), the ADR 0047 extension built as this PR's first
  commit: cross-crate edges record the target modules a path names, and a
  scoped deny fires with file:line and its reason. The reasons carry over
  verbatim.
- `text.rs` splits by consumer: `cols`/`cols_after` and the ratatui
  `prop::cols` helper move to `construct-tui/src/text.rs`; `head`/`tail`
  stay in `construct_core::text` (the TUI's two `head` calls name it
  directly). The ~40-line proptest generator (`config`, `NASTY`,
  `text_with`, `text`) is **duplicated** into `construct-tui`'s test-only
  `text::prop`, not shared via a `test-support` feature: `#[cfg(test)]`
  items cannot cross crates, and a feature that exists only to share test
  fixtures would be a public seam for private code. The copies carry the
  same fixed seed, so both sides replay the same cases.
- `libc` and `async-trait` move to `[workspace.dependencies]` (two
  consumers each: the TUI's `quiet_stderr` and the engine's sandbox/acp
  dup descriptors; both crates name async trait impls). `similar` and
  `htmd` stay in `construct-core` (non-TUI users: `tools/edit`,
  `tools/web`).
- `construct-core/build.rs`'s dirty watch and rerun-if-changed now cover
  all three shipped crates.

## Consequences

- A TUI-only edit recompiles `construct-tui` (and relinks the binary)
  while `construct-core`'s library stays cached: warm `cargo check
  --workspace` after touching `view.rs` went from 13.6 s (the core
  library rechecked) to 1.6 s. The engine's build script still reruns on
  a TUI edit (its dirty watch covers every shipped crate, so `construct
  manifest` stays honest); that git-status call is most of the 1.6 s.
  Release size and release build time are unchanged by the split itself
  (fat LTO merges everything back; ADR 0048, #271).
- `cargo tree -p construct-core --depth 1` contains none of `ratatui`,
  `crossterm`, `pulldown-cmark`, `unicode-width`; the first three are gone
  transitively too. `unicode-width` remains transitively via `secretspec`
  (`inquire` → `console`/`textwrap`, non-optional upstream) — the same
  nuance as `clap` in ADR 0048: the literal "not in `cargo tree`" is
  unachievable while `secretspec` is a dependency.
- The quick gate (`cargo xtask check --quick`) discovers `crates/*`
  (ADR 0048): `construct-tui` needed no tooling change; a TUI-only diff
  runs `test-construct-tui`.
- The duplicated generator can drift; the fixed seed and the shared
  `NASTY` pieces make drift visible as differing shrunk counterexamples,
  and a future `construct-types` split (#274's deferred knot) is the
  place to revisit sharing if a third consumer appears.
- Crate splitting pauses here until a concrete issue pulls the next seam
  (#267 codemode, A2A tools can live above `construct-core` wired into
  `Catalog` at the composition root).

## Evidence

- #277 (this change), part of #274; pure move `c56baf5` in
  `.git-blame-ignore-revs`.
- `cargo check --workspace --all-targets` and `cargo clippy --workspace
  --all-targets -- -D warnings` clean; zero visibility promotions were
  needed (the issue's "zero non-test `pub(crate)` items called by tui"
  held).
- Dev-loop: warm `cargo check --workspace` after `touch view.rs`,
  before 13.6 s / after 1.6 s (same machine, warm target dir; after the
  split the core library is not rechecked, only its build script reruns).
- The 19 `verify-construct` recipes named by the 12 affected `verified`
  feature-map entries re-driven at the rewiring commit; run ids and the
  acceptance outputs are in the PR.
