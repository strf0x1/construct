# 07 — Headless Wakes, Multi-Session Lanes, and Shared QuickJS Extensions

Status: Slice 1 (`#416`) implemented; Slices 2–6 open. Umbrella: `#391` (`#416`–`#421`). Decision
record: `docs/adr/0079-headless-wakes-and-script-extensions.md`.

## Problem

Construct's engine was built around a single interactive or prompt-driven
session. Even though background jobs can wake an idle session at a turn
boundary (ADR 0026), headless peers and standalone daemons had no way to:

1. React to schedules, sandboxed probes, external scripts, file/log events, or
   webhooks without a human typing a prompt.
2. Route autonomous events into isolated lanes (`fresh`, `dedicated`, `shared`)
   without colliding with interactive turns or holding every idle `Agent` in
   memory forever.
3. Run zero-token programmable filters (`#391`), Code Mode scripts (`#267`),
   and learned script tools (`#186`) through one confined, capability-scoped
   runtime without linking a JS engine into bare/lean builds.

## Architecture

### Layer 1: Core wake seams & multi-session manager (`construct-core`, Slice 1 / `#416`)

- **`construct-core::wake`**:
  - Plain data types: `WakeEvent`, `WakeTrust` (`Trusted` | `Untrusted`),
    `SessionPolicy` (`Fresh` | `Dedicated(String)` | `Shared`).
  - Extensible traits mirroring `ToolSource` (ADR 0056): `WakeSource` (event
    producers), `WakeFilter` (zero-token pre-wake filters/correlators), and
    `WakeSources` composed at `crates/construct/src/cli/sources.rs`.
  - `WakeBus`: centralizes filtering, deduplication (`dedup_key`), burst
    coalescing (`coalesce`), cooldown rate limiting (`cooldown`), TTL expiry,
    and `<wake id="…" source="…" kind="…" untrusted="…">` fencing with
    closing-tag escape (`<\/wake`).
  - Built-in zero-dep sources: `timer`, sandboxed `command` probe, and `cli`
    over `$CONSTRUCT_HOME/wake.sock` (`0600` JSON-lines).
- **Additive journal vocabulary (`event.rs`, `journal/browser.rs`)**:
  - `Event::WakeReceived` and `Event::WakesDelivered` (one generic pair for all
    wake sources; `WakesDelivered.message` replays verbatim on resume).
  - `Event::Head.origin: Option<SessionOrigin>` (`interactive` | `wake { source,
    trigger }`).
  - `journal::browse` surfaces wake-initiated sessions (even with zero user
    `Submitted` prompts); `journal::find_dedicated` locates `Dedicated` lanes
    for SQLite WAL rehydration.
- **Unified turn-boundary delivery (`agent/wake.rs`)**:
  - One choke point delivers both `jobs::Status::wakes()` completions and
    `WakeEvent`s strictly at turn boundaries, exactly once, with per-trigger
    learning opt-out. `WakeReceived` is journaled when a wake reaches its
    session and `WakesDelivered` at the boundary, so a process that dies in
    between finds the wake again on resume (`wake::unconsumed`) unless it is
    older than its TTL; the next wake turn on that lane carries it along.
- **Multi-session `SessionManager` (`sessions/`) & ACP peer parity (`acp/server/wakes.rs`)**:
  - Routes wakes across `fresh`, `dedicated`, and `shared` lanes under a
    bounded concurrent active-turn cap (`[wake].max_concurrent_turns`) and
    evicts idle agents after `[wake].idle_evict`.
  - Both `construct watch` (+ `construct wake emit|list`) and `construct peer`
    run the exact same declarative `[[wakes]]` config schema (including stamped
    peer `[config]`), with autonomous execution continuing while
    `Input::Detached` and background shell jobs waking idle ACP sessions.

### Layer 2: Shared QuickJS extension runtime (`crates/construct-script/`, Slice 2 / `#417`)

- Feature-gated behind `--features script` (in `full`, excluded from bare/lean
  by default; `construct-core` never depends on `construct-script`).
- Confined `rquickjs` sandbox with zero ambient authority (no `fs`, `net`,
  `process`, `env`, `eval`, dynamic `Function`, OS timers), bounded memory/CPU
  interrupts, and SQLite-backed per-script KV (`ctx.state`).
- One capability-scoped host bridge (`ctx`) shared by wake scripts (`#391`
  implementing `WakeFilter` / `WakeSource`), learned tool scripts (`#186`
  implementing `ToolSource`), and Code Mode (`#267`).
- Atomic turn-boundary / between-firing hot-reload with last-known-good
  fallback and `Operator` vs. approval-gated `AgentProposed` trust tiers.

### Layer 3: Hub lanes, concrete sources, webhooks & live push (Slices 3–6 / `#418`–`#421`)

- `#418`: Multi-session lanes in `construct-hub` (`registry.rs`, `remote.rs`),
  TUI attention indicators, and per-peer proxy budget/rate caps.
- `#419`: Feature-gated `file` and `log` watch `WakeSource`s.
- `#420`: Hub-terminated authenticated webhooks relayed over Noise `Channel::Wake`.
- `#421`: Agent `wake` tool (ephemeral vs. approval-gated persistent) and
  `construct hub script push`.

## Staged Verification

- **Slice 1 (`#416`)**: `cargo xtask check --affected`, `lean` release/check
  build, unchanged golden request snapshots for interactive sessions without
  wakes, and `crates/construct-dev/tests/wakes.rs` covering `fresh`/
  `dedicated` (with idle eviction + SQLite rehydration)/`shared` routing,
  coalescing, cooldown, untrusted `</wake>` fencing, `construct watch` +
  `construct wake emit` + `timer`, journal rows, and `construct peer` waking
  autonomously while detached from the hub.
