# 0062 — The `arch` gate ratchets every module cycle, not only pairs

Status: accepted, 2026-11-21. Follows 0060.

## Context

`docs/ratchets.toml` `[pairs]` lists the module pairs that use each other
both ways, and the `arch` gate fails on a new one. A cycle needs no such
pair: `a -> b -> c -> a` is invisible to it. ADR 0060 found exactly that in
`construct-core`, a 16-module strongly connected component standing while
the only pairs left were the two ADR 0057 accepts, and named a ratchet on
cycles as the follow-up.

The options were the size of each crate's largest component (one number), or
the members of every component. A size misses a second, separate cycle and a
same-size cycle that swaps a module; members miss nothing and show in a diff
which module joined or left.

## Decision

`[cycles]` in `docs/ratchets.toml` lists, per crate, every strongly connected
component of three or more modules as its sorted names joined by `, `, found
by the same scan as `[pairs]` and `construct-dev code-graph`. A component of
two is a pair, which `[pairs]` already reports with where each edge is, so it
is not listed twice. It must match the scan exactly:

- a component not listed fails: as a new cycle (including a pair that a
  third module joined), or as a listed one that grew (naming the modules that
  joined, and both when two merged);
- a listed component that shrank fails, naming the smaller one to list;
- a listed component that is gone, or down to a pair, fails until it is
  deleted.

`[pairs]` stays: it says where each direction of a two-way edge is, which a
component list can't.

## Consequences

- A change that closes a loop through any number of modules now fails the
  gate the way a new pair does, and an intended one is a reviewed line in
  `ratchets.toml`.
- Baselines at the time of writing: `construct-core` `config, endpoint,
  models, providers` (intended, ADR 0057/0060); `construct-dev` `arch,
  feature_map, xtask`; `construct-tui` `app, ask, palette, picker, prompt,
  sessions, view`.
- The `construct-tui` component is larger than its three pairs suggested.
  Most of it hangs on `view`: `palette` and `picker` call `view::fit`
  (`palette.rs:223`, `picker/draw.rs:8`), and `view` uses `app`
  (`view.rs:4`). Moving `fit` out of `view` is a likely cheap cut; not done
  here.
- `construct-dev`'s component is `xtask` calling each gate's `problems` and
  the gates calling `xtask::rust_sources`, a shared helper.

## Evidence

- `construct-dev code-graph .` at the branch point (`26fc093`): the three
  components above, and the `construct-tui` edges among them listed in the
  PR.
- Unit tests in `crates/construct-dev/src/arch/tests.rs` cover a new
  three-module cycle with no pair, growth, shrinking and removal.
