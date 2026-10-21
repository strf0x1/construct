# 0005 — Tool groups only grow within a session

Status: accepted (backfilled 2026-10-21).

## Context

The first request is kept small: core tools only, everything else routed per
turn (AGENTS.md). The router can switch a group on for one turn and off the
next, which keeps the tool list minimal but changes the `tools` array between
requests. Providers cache a prompt prefix and the tool definitions are part of
it, so a list that shrinks or reorders invalidates the cache for the rest of
the conversation.

## Decision

Once a group is on for a session it stays on. The tool list sent to the model
only ever grows. A group is enabled by the
router for a turn, or by the model itself through the `enable_tools` tool when
the router missed (`ENABLE_TOOLS` in `crates/construct/src/tools/mod.rs`, answered
in `agent.rs`, not a catalog tool). Nothing disables a group mid-session.

## Consequences

- Provider prefix caches stay warm after the first enable; later turns pay only
  for what grew.
- A router false positive costs the schema tokens for the rest of the session.
  This is why the router asks strict questions (ADR 0003) and why routing
  accuracy is scored on false positives too.
- Revisit if a provider is found where a changing tool list does not hurt, or
  if a session's accumulated schemas become large enough to outweigh the cache
  benefit. We have not measured either.

## Evidence

The design is stated in the `crates/construct/src/tools/mod.rs` module header
and is observable in `crates/construct-dev/tests/e2e.rs` (first request
carries `shell, read, write, edit, grep, glob, enable_tools`; `enable_tools`
adds a group from the next step) and in the golden snapshots in
`crates/construct-dev/tests/snapshots.rs`.

No bench result isolates this decision: there is no recorded run with groups
switching on and off against groups only growing, so the cache benefit above
is the stated rationale, not a measured one. In `routing-20261020-111915.json`
the model loaded a skill via `enable_tools` at call 14–21 in 4 of 6 local-router
runs, which shows the escape hatch is used when routing misses.
