# 0056 — Tools can live above construct-core

Status: accepted. 2026-11-20. Part of #303 (step 11, #314); the plan is #274.

## Context

`tools` is about 11k of core's lines with a fan-out of 15, and every tool had
to live inside the engine because `Catalog::build_with` chose the
implementations itself. #274 plans optional tool crates wired in at the
composition root; with no registration seam, none could exist.

## Decision

- `construct-core` offers `tools::ToolSource`: a factory asked once per
  catalog, `tools(&ToolsConfig, &Deck) -> Result<Vec<Arc<dyn Tool>>>`.
  `ToolSources` is the cheap-to-clone list a session is built with;
  `Agent::new_with_tools` and `Catalog::build_extended` take it, and
  `acp::server::run`, `construct_tui::run` and `headless::run` carry it down
  from `crates/construct/src/cli/sources.rs`, the one place that registers.
- A source is a factory, not a `Vec<Arc<dyn Tool>>`, because a tool can
  depend on the surfaces a catalog is built for (the `deck` tool needs
  `Deck::control`), and catalogs are built per session. The learner's
  catalog, `construct manifest` and core's own tests are built without
  sources and get none.
- The extras are added inside `build_extended`, after the built-in pushes and
  before the duplicate-name and `[tools] disabled` checks. Adding them after
  those checks would make `disabled = ["deck"]` fail with "unknown tool" and
  let an extra tool silently take a built-in's name. They also stay last, so
  request and manifest order do not change.
- `tools::def` and `tools::obj` are public: they are the one way a tool shapes
  its schema, and a tool crate has nowhere else to get them.
- The proof is the coordinator's `deck` tool, moved to the new
  `construct-deck` crate (`control` was the last tool pushed, referenced from
  core only by the catalog, and Deck-only). `Group::Coordinator`, the wire
  name, schema and routing are unchanged. Its contract test moved with it.
- `construct-deck` is `used_only_by` the binary in `docs/architecture.toml`:
  the engine never names it.

## Consequences

- The binary crate stays test-less (ADR 0048); tool code and its tests live
  in a library crate.
- `glob`, `memory` and the other default tools stay in core: moving one would
  make every `Catalog::build` caller pass sources or lose a default tool.
- #315 cannot be a pure move for the other Deck tools. `boundary` is a field on
  `Ctx` and is used by `acp` and `agent`; `developer` is imported by
  `shell_exit`. Each needs a cut first (a per-session slot a tool crate can
  own, and inverting `shell_exit`'s use of `developer`).
