# 0058 — `developer` and the boundary tools live in `construct-deck`

Status: accepted. 2026-11-20. Part of #303 (step 12, #315); builds on ADR 0056.

## Context

ADR 0056 added `ToolSource` and moved the coordinator's `deck` tool out of
core, and said #315 could not be a pure move: `boundary` was a field on
`Ctx` read by `acp` and `agent`, and `shell_exit` imported `developer`.
`developer` (3.1k lines), `boundary` (0.7k) and the earlier `control` were
4.7k of `tools`' 11.2k.

## Decision

- `developer`, the three boundary tools and Deck's boundary client move to
  `construct-deck`, and `DeckTools` registers them in the order core pushed
  them (`developer`, `boundary_wait`, `boundary_status`, `boundary_withdraw`,
  then `deck`), so request and manifest order are unchanged. `developer` is
  not Deck-only: it is the host's approved git tool and `DeckTools` always
  returns it. A catalog built without sources (the learner's, core's own
  tests) no longer has it.
- `Ctx.boundary` is `Option<Arc<dyn BoundaryHost>>`, a trait in
  `tools/boundary.rs` with the five things the engine needs: the gateway
  sequence around a tool, the late-decision queue, and "seen". The ACP server
  hands a `_deck/boundary` payload to each session's host as raw params; the
  host parses, validates and filters it. `ToolSource::boundary(url)` builds the
  host, `restore.rs` asks the sources for it, and the boundary tools get the
  concrete client back through `BoundaryHost::as_any`.
- `shell_exit` finds the linked worktree and asks `ToolSource::worktree_note`
  what to say; `DeckTools` answers with the `developer` operations. Core
  names no `developer` wire string. `Ctx` carries the sources for this.
- `tools::arg`, `tools::num` and `sandbox::Policy::real` become public: the
  helpers a tool crate has nowhere else to get.
- No ACP wire change: the capability, the method names and the payloads are
  the ones the vendored contract pins.

## Consequences

- `tools` drops from 9.8k to 6.1k lines (-38%).
- `tools→http` and `tools→secrets` do not go away, contrary to the issue's
  hope: `Ctx` builds the shared client and `web` resolves a search backend's
  key. Only the gateway client and `developer`'s secrets check left.
- Adding another Deck surface that core must react to means a method on
  `BoundaryHost` or another `ToolSource` hook, never a core import of
  `construct-deck` (`used_only_by` in `docs/architecture.toml`).
