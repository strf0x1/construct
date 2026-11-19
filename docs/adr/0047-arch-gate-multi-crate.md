# 0047 — arch gate: per-crate tables and cross-crate edges

Status: accepted. 2026-11-19.

## Context

Crate modularization (#274) moves modules out of `crates/construct/src` in
steps: the composition root (#276), then `construct-tui` (#277), then
issue-driven splits. The `arch` gate as written fails on any move: it scans
one source root (`crates/construct/src`) against a flat `[modules.*]` table,
so an entry for a gone module and a missing entry for a new crate both fail
before anything can move. #275 makes the gate multi-crate first, on the
unchanged tree.

The schema options were per-crate tables (`[crates.construct.modules.tui]`
plus crate-level rules) or crate-qualified module names in the existing flat
table. The flat table would mix two kinds of edge (module and crate) in one
namespace and make crate-level deny rules awkward to name; per-crate tables
keep each crate's graph readable on its own and give crate rules a home.

## Decision

`docs/architecture.toml` is a table of crates, keyed by package name:

- `[crates.<package>]` carries the crate-level rules: `may_use_crates`
  (workspace crates this crate may name), `must_not_use_crates`
  (`{ crate, reason }`), and `used_only_by` (`{ crates, reason }`).
- `[crates.<package>.modules.<m>]` carries the module rules exactly as
  before: `may_use`, `must_not_use`, `used_only_by`.

The scanner reads every `crates/*/src`. The package name comes from each
crate's `Cargo.toml`; the crate ident is that name with `-` turned into `_`.
A package's lib and bin roots share one module pool: `main.rs` is `main`,
`lib.rs` is `lib`, and a module declared only in `main.rs` (construct-dev's
`arch`, `xtask`, …) is a top-level module like any other. A path naming the
package's own lib (`construct::x` in construct, `construct_dev::x` in
construct-dev) is a module edge — this generalizes the old `main.rs`-only
special case. A path naming another workspace crate is a cross-crate edge,
recorded crate-to-crate at the line of the crate name.

Every existing invariant carries over and extends to crates: an unlisted
edge fails, a listed edge that no longer occurs fails, a crate or module
without an entry fails, an entry for a gone crate or module fails, a deny
rule without a reason fails, and zero edges found anywhere fails so a broken
lexer cannot pass. Deny and `used_only_by` violations print file:line and the
reason for every occurrence; an unlisted edge is reported once per pair.

## Consequences

- Steps 2–3 of #274 are table edits: `cli`/`main` entries become the binary
  crate's section, `tui`'s `used_only_by = ["cli"]` and its `must_not_use`
  rules become crate-level rules on `construct-tui`. A new crate cannot
  appear silently; the gate demands its section.
- `construct-dev` is gated too (it is a workspace crate): 13 module entries,
  migrated from the scanner's own report of the real edges.
- Cross-crate edges are crate-granular: there are no module-level rules
  about another crate's internals. If a later split needs one (a types crate
  exposing only `Event`, say), extend the schema then.
- The pooled lib/bin module model means the gate can list an edge cargo
  would reject (a bin-only module naming a lib module through `crate::`).
  Cargo stays the compile authority; the table governs what may exist.
- Unchanged limitation: bare qualified paths without `crate::` or a `use`
  (construct-dev `main.rs` calling `xtask::check`) are not counted. The
  table is authoritative and the scan indicative, as before.
- No cross-crate edge exists in non-test code today (`construct` is only a
  dev-dependency of `construct-dev`, used in tests, which are stripped), so
  both `may_use_crates` lists are empty until #276 creates the first real
  one. The cross-crate machinery is proven by gate tests and a scratch
  experiment instead.

## Evidence

- #275 (this change), part of #274.
- `cargo xtask check` green on the unchanged source tree with the migrated
  table; 22 arch gate tests, including unlisted/removed cross-crate edges,
  crate deny and `used_only_by` violations, and missing/stale crate entries.
- Scratch experiment (not committed): a probe file in `construct-dev/src`
  with `use construct::agent::Event;` failed the gate with
  `zz_probe.rs:2: construct-dev must not use construct
  (docs/architecture.toml): <reason>` under a crate deny rule, and with the
  `may_use_crates` one-line-edit message without it.
