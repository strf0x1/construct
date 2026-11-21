# 0057 — `config ↔ providers` and `config ↔ models` are intended pairs

Status: accepted. 2026-11-20. Part of #303 (step 14, #317).

## Context

After #303's earlier steps, the only two-way module pairs left in `construct-core` are
`config <-> models` and `config <-> providers` (`docs/ratchets.toml`). #303
is done when every remaining pair is either cut or written down as intended.
#317 asked which of these it is: keep the pattern behind them, or move to
free functions over a narrower view of config.

The pattern: `providers/resolve.rs`, `models/select.rs` and `models/mod.rs`
each open an `impl Config` block. Rust lets any module of the crate that
defines a type add inherent methods to it, so resolution lives with the
module that owns it (ADR 0027) while callers still write `cfg.resolve(x)`
or `cfg.endpoint()`.

`construct-dev code-graph` shows that the `impl` blocks are only half of
each pair. Every reference among the three modules:

| Edge | Site | Names |
|---|---|---|
| config → providers | `config.rs:5` | `CacheMode`, `Dialect`, `ProviderConfig`, `Reasoning` (re-export) |
| config → providers | `config.rs:57` | `Provider` (field type) |
| config → models | `config.rs:61`, `:68` | `ModelEntry` (field types) |
| config → models | `config.rs:64` | `Defaults` (field type) |
| models → config | `models/mod.rs:14` | `Config`, `CacheMode`, `Dialect` |
| models → config | `models/select.rs:11` | `Config` |
| providers → config | `providers/resolve.rs:10` | `Config` |

The downward edges exist because `Config`'s fields use types those modules
define. The upward edges exist because those modules read and change
`Config`: `ModelEntry::apply` and `select_model` write `context_window`,
`max_tokens` and `defaults`, and `endpoint()` builds from the declared
fields as a whole.

Two options were on the table:

- **A: accept the pattern.** Record the pairs as intended.
- **B: free functions over a narrower view.** `fn resolve(cfg: &Config, …)`
  still names `Config`, so the upward edges stay and neither pair is cut.
  A narrower view does not save it: `select_model` lays an entry's limits
  over the top-level fields, so a view big enough for it is most of
  `Config`. B would change about 100 call sites across `construct-core`,
  `construct` and `construct-tui` (`select_model` 22, `endpoint` 18,
  `resolve` 12, `wire_id` 10, …; counted with grep, so test code included),
  and the pair count would not move.

## Decision

Option A. `config <-> models` and `config <-> providers` are intended pairs.

- Configuration data is typed by the modules that own its behavior
  (`providers::Provider`, `models::ModelEntry`, `models::Defaults`), and
  those modules add methods to `Config` with `impl Config` blocks (ADR 0027).
  Both directions are part of the design, not debt.
- They stay listed under `[pairs] construct-core` in `docs/ratchets.toml`,
  with a comment pointing here. The ratchet still fails on any new pair, and
  on either of these if it is cut and not deleted.
- New `Config` behavior that belongs to providers or models goes in their
  `impl Config` blocks, not in `config.rs`.
- Do not make the gate stop reporting these pairs by cutting only what it
  can see. `Config::load` (`config.rs:201-205`) calls `load_catalog`,
  `check_providers` and `select_model`, which are defined in `models` and
  `providers`. The scan follows paths, not method calls, so moving the field
  types into `config` would make the gate show one-way edges while the
  dependency still runs both ways. Cutting a pair means removing the
  dependency, not the path that names it.

## Consequences

- Nothing changes in the code, and the golden snapshots and reference counts
  are unchanged. With this ADR, every pair left in `construct-core` is
  intended, which meets #303's "cut or written down as intended" for that
  crate.
- The three modules form a closed cycle of their own. `endpoint` is used by
  `models` and `providers` but does not use them back, and `event`,
  `journal` and `llm` are outside the cycle, so #303's done-when does not
  depend on it.
- What would make us revisit it: a plan to move `config` into its own crate.
  Cargo forbids crate cycles, and an inherent `impl Config` will not compile
  outside the crate that defines `Config`. The way out then is mechanical:
  each `impl Config` block becomes an extension trait (`pub trait Resolve {
  fn resolve(&self, …) }` in `providers`, implemented for `Config`), and
  call sites keep `cfg.resolve(x)` after a `use providers::Resolve;`.
  `Config::load`'s calls into `models` and `providers` would move up a
  layer. #274's `construct-types` holds `Event`, `Message` and tool shapes,
  not config, so nothing plans that today.
- A side effect of the re-export on `config.rs:5`: `llm/dialect.rs`,
  `llm/overlay.rs`, `endpoint.rs`, `manifest.rs` and `models/mod.rs` import
  `CacheMode`, `Dialect`, `ProviderConfig` or `Reasoning` through `config`,
  although `providers` defines them. The code graph shows those as edges to
  `config`. Whoever splits `llm` or `journal` out (#274) should read them as
  edges to `providers`.

## Evidence

- #317 (this decision), #303 (the plan), ADR 0027 (resolution lives with its
  owner), ADR 0047 (the arch gate and its pair ratchet).
- `cargo run -q -p construct-dev -- code-graph` on `origin/main` at the time
  of writing: the table in Context is every edge among `config`,
  `providers` and `models`. Five references point down from `config`, three
  point back up.
