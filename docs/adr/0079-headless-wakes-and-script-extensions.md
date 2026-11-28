# 0079 — Headless wakes, multi-session lanes, and a shared QuickJS extension runtime

Status: proposed. 2026-11-28. Part of #391 (#416–#421); aligns #267, #186, #272
and #143. Extends ADR 0026; builds on ADR 0008, 0025, 0029, 0048, 0056, 0064,
0067, 0068, 0071, 0072 and 0075.

## Context

Today construct is a prompt-driven single-session harness. A background job can
wake an idle session at a turn boundary (`agent/wake.rs`, `jobs::Status::wakes`,
ADR 0026), and the coordinator's `peer` tool uses `Kind::Remote` on that same
queue (ADR 0075). Everything beyond that assumes a human or client starts each
turn:

1. **No autonomous wake loop on headless peers.** `construct peer` runs
   `acp::server::serve`, which already holds `sessions: HashMap<String, Session>`
   but only reacts to incoming JSON-RPC lines — it never polls `jobs.ready()` or
   starts a turn on its own.
2. **One session per peer at the hub.** `construct-hub`'s `registry.rs` and
   `remote.rs` persist a single `session`, `turn` and `prompt` per peer and open
   the session lazily on the first hub `session/prompt` (ADR 0071). An
   autonomous wake has nowhere to land without colliding with interactive work
   or stalling it on an unanswered 3 AM permission question.
3. **Single-session assumptions in the journal and proxy.** `journal/browser.rs`
   hides any session with zero user `Submitted` prompts; recovery defers
   unconsumed completions until a human prompts (ADR 0029); `proxy.rs` caps
   in-flight concurrency (`MAX_INFLIGHT = 4`) and counts usage in memory, with
   no per-peer spend or rate cap (ADR 0072).
4. **Three scripting use cases are converging.** `#391` needs programmable
   event filters (running at zero LLM tokens so noisy logs or periodic timers
   only wake a model when a threshold or stateful condition trips), `#267` (Code
   Mode) runs short ad-hoc JS programs over a turn's tools, and `#186` (learned
   script tools) promotes repeated multi-call sequences into named routed tools.
   Building three separate script runners would duplicate sandboxing, host
   bindings and binary weight.

Prior art checked before deciding:

- **OpenCode 2 and Psychoscript (`anomalyco/psychoscript`)**: OpenCode 2
  hot-reloads local plugins, skills and MCP config live at safe boundaries
  without restarting active sessions (`docs/research/plugin-builds-and-reload.md`).
  Psychoscript runs a confined JS-subset bytecode VM with zero ambient OS
  authority (all I/O goes through explicit typed host functions) and
  serializable mid-flight snapshots (~5 KB) for pausing long Code Mode scripts
  across restarts (`docs/research/codemode-other-harnesses.md`).
- **What translates to Rust/construct and what does not**:
  - Steal the **capability-based host bridge** (zero ambient authority) and
    **atomic boundary hot-reload** with last-known-good fallback.
  - Do **not** build a custom resumable JS bytecode VM upfront. Trigger handlers
    (`onEvent(event, ctx)`) finish in milliseconds and keep continuity in a tiny
    persisted per-script SQLite KV store (`ctx.state`), while crashed tool calls
    already follow ADR 0029 (marked interrupted, never auto-replayed).
  - Between `rquickjs` (QuickJS), Starlark, WASM components and sandboxed
    subprocesses: subprocesses assume Python/Node on stripped target boxes and
    make in-process `Catalog` callbacks awkward; WASM requires a compile step
    and heavy runtime; Starlark lacks `async/await` and models write JS best.
    `rquickjs` is already explored in `#267` and stays out of bare/minimal peers
    by living behind a Cargo feature flag.

Constraints:

- `construct-core` stays pure Rust with no `rquickjs`, `notify` or HTTP server
  dependencies, so `lean` and bare stamped peer builds pay zero size or C-build
  cost unless opted in (ADR 0048, 0052, 0064).
- `Event` remains additive-only (ADR 0008); adding a new wake source must not
  require a new `Event` enum variant across journal, ACP, TUI and triage tools.
- One wake authorization choke point stays in place (ADR 0026); untrusted
  payloads are fenced before reaching the model (ADR 0075).
- Dropped peers never open inbound listening ports (ADR 0068, 0070).
- Nothing here starts an ACP contract change (`sprawlcommons-acp`) unless a
  later hub/ACP surface explicitly requires one.

## Decision

### 1. Two-layer wake architecture (one choke point)

- **Instance-level wake bus (`construct-core::wake`).** Core defines the plain
  data types and traits, mirroring `tools::source::ToolSource` (ADR 0056):
  - `WakeEvent`: `id`, `source_kind`, `source_name`, `dedup_key`,
    `occurred_at`, `trust: WakeTrust` (`Trusted` | `Untrusted`), `summary`, and
    structured `data: Value`.
  - `SessionPolicy`: `Fresh` (new session per wake), `Dedicated(String)` (one
    persistent session per named trigger, rehydrated across restarts) or
    `Shared` (the instance's interactive/main session).
  - `WakeSource` (event producer) and `WakeFilter` (optional pre-wake script or
    matcher), composed via `WakeSources` at `crates/construct/src/cli/sources.rs`.
  - `WakeBus` centralizes all cross-cutting behavior so no source reimplements
    it: burst coalescing and debouncing, cooldown and rate limiting,
    deduplication, and `<wake source="…" kind="…" untrusted="…">` fencing with
    closing-tag escaping (matching `jobs::render`).
- **Session-level turn-boundary delivery (`agent/wake.rs`).** Generalize the
  session wake inbox so both `jobs::Status::wakes()` endings and routed
  `WakeEvent`s pass through one delivery choke point: turn-boundary only,
  journaled, and exactly once.

### 2. Additive journal vocabulary and session discovery

- **One generic additive `Event` pair (`event.rs`).** Add `Event::WakeReceived`
  and `Event::WakesDelivered` carrying `kind: String`, `source: String`,
  `untrusted: bool` and `data: Value` (the ADR 0025 `Event::Note` pattern). New
  wake sources add data values, never enum variants.
- **Session origin (`Head.origin`).** Add an optional additive
  `origin: Option<SessionOrigin>` (`interactive` | `wake { source, trigger }`)
  on `Event::Head` (the ADR 0067 `fork_of` pattern).
- **Discovery, recovery and learning.**
  - `journal/browser.rs` surfaces wake-initiated sessions (even with zero
    `Submitted` user prompts) and labels them by origin; `Dedicated` lanes
    locate their existing session by `Head.origin`.
  - Recovery applies a configurable wake TTL/expiry policy on restart so a
    rebooted headless instance does not stampede stale queued ticks.
  - Wake definitions may throttle or disable background learning (`learn/`) so
    routine quiet checks do not churn the learner.

### 3. Multi-session `SessionManager`, standalone CLI and peer parity

- **`SessionManager` (`construct-core`).** Owns active sessions for headless
  hosts, routes wakes according to `SessionPolicy`, enforces a bounded
  concurrent-turn limit, and evicts idle `Agent` instances from memory after an
  idle TTL (transparently rehydrating them from SQLite WAL on the next wake or
  prompt via existing per-session advisory locks, ADR 0029).
- **One schema, two hosts.** Declarative `[[wakes]]` entries in `config.toml`
  (and in a stamped peer's `[config]`, ADR 0073) work identically in:
  - **Standalone headless mode**: `construct watch` (runs `SessionManager` +
    `WakeBus` locally without a hub) plus `construct wake emit` / `construct
    wake list` for debugging and external-script integration.
  - **`construct peer`**: `acp::server::serve` runs over the same
    `SessionManager` + `WakeBus`, so local `[[wakes]]` (and background shell
    jobs) wake sessions autonomously even while `Input::Detached` (ADR 0071).
- **Declarative wakes need no JS engine.** `[[wakes]]` with `match`,
  `coalesce`, `cooldown`, `session` and `prompt` works in builds compiled
  without `script`.

### 4. Shared feature-gated QuickJS runtime (`crates/construct-script/`)

- **Crate and feature flag.** Add `crates/construct-script/`, wired in at the
  composition root behind `--features script` (in `full`, opt-in via
  `--features lean,script`, excluded from bare peer builds by default).
  `construct-core` never depends on `construct-script`.
- **Confined sandbox.** Built on `rquickjs` with zero ambient authority: no
  `fs`, `net`, `process`, `env`, `eval`, dynamic `Function` or OS timers. Host
  config enforces memory ceiling, CPU interrupt timeout, child call limit,
  output byte cap and cancellation propagation.
- **Persisted per-script state.** Each script gets a bounded key-value store
  (`ctx.state`: `get`, `set`, `delete`) backed by SQLite in `$CONSTRUCT_HOME`,
  providing cross-tick and cross-restart state without frozen VM call stacks.
- **One capability-scoped Host Bridge (`ctx`) across all three roles:**
  - **Wake scripts (`#391`, `$CONSTRUCT_HOME/scripts/wakes/`,
    `.construct/scripts/wakes/`):** `export const wake = { on, session,
    cooldown, … }` and `export async function onEvent(event, ctx)`. `ctx`
    exposes `ctx.state`, `ctx.wake({ session?, summary, data, untrusted? })`,
    `ctx.log` and policy-gated read-only `ctx.tools`. Runs at zero LLM tokens;
    an agent turn starts only when `ctx.wake` is called.
  - **Tool scripts (`#186`, `$CONSTRUCT_HOME/scripts/tools/`,
    `.construct/scripts/tools/`):** `export const meta = { name, description,
    schema }` and `export async function run(args, ctx)`. `ctx` exposes
    `ctx.tools` (`list`, `describe`, `call` through `Catalog::call_with` with
    normal `Perms`, sandbox and `parent_call_id` journaling) and optional
    `ctx.state`.
  - **Code Mode (`#267`):** fresh QuickJS context per call using the same
    `ctx.tools` bridge scoped to the turn's enabled tool groups.

### 5. Atomic boundary hot-reload and trust tiers

- **Boundary swaps only.** Script changes take effect only between turns (for
  tools) or between event firings (for wakes). Active executions finish on the
  snapshot they started with; tool definitions never mutate mid-step.
- **Last-known-good fallback.** A parse or validation error keeps the prior
  working script active and emits a structured `Event::Note` (surfaced in
  CLI/TUI status) instead of crashing the process or dropping active monitors.
- **Trust tiers.**
  - `Operator` scripts (placed in `$CONSTRUCT_HOME/scripts/` or pushed by the
    hub in `#421`) hot-load automatically.
  - `AgentProposed` scripts (written by the learner or an agent tool) and
    persistent agent-created wakes never activate silently: they require
    fixture/dry-run validation and explicit human/hub approval. Ephemeral
    agent-created wakes are session-scoped and die with the process.
- **Shared rule for MCP hot-reload (`#143`).** Reloading MCP servers follows
  the same turn-boundary swap, last-known-good fallback and permission-gate
  contract.

### 6. Hub multi-session peers, webhooks and budget caps

- **Multi-session peers and unsolicited turns (`#418`).** `construct-hub`
  (`registry.rs`, `remote.rs`) supports peer-initiated wake turns alongside
  hub-initiated prompts, tracks multiple session lanes per peer (`construct hub
  sessions <peer>`, `construct hub session <peer> [--session <id>]`), and
  surfaces errors, open permission questions and explicit escalations as
  attention indicators in `construct hub status` and the TUI peer attach view
  without blocking the interactive lane.
- **Hub-terminated webhooks (`#420`).** Authenticated HTTP webhooks (Bearer or
  HMAC-SHA256, strict body and rate limits) terminate at `construct-hub` and
  relay to pinned peers over the encrypted Noise link on a new `Channel::Wake`
  (tag `4` in `construct-peer/src/envelope.rs`, outside `sprawlcommons-acp`,
  matching `Channel::Llm` in ADR 0072). Standalone `construct watch` may expose
  an opt-in `127.0.0.1` loopback listener under the same auth and fencing.
- **Per-peer spend and rate caps (`#418`).** `construct-hub/src/proxy.rs`
  enforces configurable per-peer request-rate, token or USD window budgets
  (answering `429` before going upstream), mirrored by local rate/budget limits
  in `construct-core` for `llm = "local"` peers and standalone `construct watch`.

### 7. Staged implementation slices

1. `#416` — Multi-session headless core, `WakeBus`, generic `Event`,
   `SessionManager`, `construct watch` / `construct wake emit`, `timer` and
   `cli`/`command` sources.
2. `#417` — `crates/construct-script/` (`--features script`), confined QuickJS,
   unified `ctx` Host Bridge, SQLite KV `ctx.state`, and boundary hot-reload.
3. `#418` — `construct-hub` multi-session peers, unsolicited wake turns, TUI
   escalations/lane switching, and per-peer proxy budget caps.
4. `#419` — Feature-gated `file` and `log` watch wake sources (rotation-safe
   cursors, burst coalescing, untrusted fencing).
5. `#420` — Hub-terminated authenticated webhooks over `Channel::Wake`.
6. `#421` — Agent `wake` tool (ephemeral vs. approval-gated persistent) and
   `construct hub script push` live hot-reload to running peers.

## Consequences

- **Easy:** Adding a new wake source is one `WakeSource` implementation (or a
  user `[[wakes]]` entry / `construct wake emit` script) with zero `Event` enum
  churn. Zero-token JS pre-filters keep headless peers cheap and quiet. Code
  Mode (`#267`), learned tools (`#186`) and event scripts (`#391`) share one
  sandbox, one host bridge and one LLM-familiar JS idiom. Bare/lean peer builds
  stay small and C-free when `script` is off.
- **Hard:** `construct-hub`'s `Remote` state machine and TUI peer attach view
  grow from one session per peer to multiple lanes. `rquickjs` introduces a C
  dependency under `--features script` that must stay clean under `cargo deny`
  and Linux/macOS/musl release targets.
- **Ruled out:** Mid-flight JS VM call-stack snapshots across restarts; ambient
  filesystem/network/process access from scripts; silent persistent wake or
  script activation by an agent; inbound webhook ports on dropped peers;
  linking `rquickjs` or `notify` into `construct-core`.
- **Revisit if:** Long-running multi-hour scripts genuinely need mid-flight
  pause/resume across process restarts (beyond `ctx.state` KV + wake events),
  `rquickjs` proves untenable for static musl peer builds (in which case the
  script backend trait can swap to a pure-Rust VM without changing the module
  or host-bridge contract), or an ACP client outside `construct-hub` needs
  multi-session peer lane discovery (which would start in `sprawlcommons-acp`).

## Evidence

- Umbrella and slices: #391, #416, #417, #418, #419, #420, #421.
- Aligned issues: #267 (Code Mode), #186 (learned script tools), #272
  (modularity/plugins), #143 (permission-gated MCP hot-reload).
- Prior art and repo research:
  - `docs/research/codemode-other-harnesses.md` (Droid 1.0, OpenCode 2.x
    `psychoscript`, and construct's QuickJS prototype).
  - `docs/research/plugin-builds-and-reload.md` (Cargo feature splits,
    OpenCode 2 / OpenChamber boundary hot-reload, and learned script tool
    validation).
  - `anomalyco/psychoscript` (confined bytecode VM with zero ambient authority
    and typed host functions).
- Not yet measured: release binary size delta for `--features script`, RSS under
  idle session eviction, and tier-2 Code Mode / learned-tool token savings
  (tracked in `#416`, `#417`, `#267` and `#186`; results will be recorded as
  slices land).

## Slice 2 as built (#417)

The first PR ships the wake-script role only; tool scripts (#186) and Code
Mode (#267) reuse its `sandbox`, `bridge` and `reload` modules later. Choices
made along the way that later slices must keep:

- **No compiler at run time.** QuickJS cannot compile source without its
  `Eval` intrinsic, and with it `eval`, `new Function` and
  `(async () => {}).constructor` can't be blocked from JS. So a script is
  compiled to bytecode in a throwaway context that has `Eval`, and runs from
  that bytecode in a fresh runtime whose context does not
  (`construct-script/src/sandbox.rs`). There is no module loader, so `import`
  fails. Each call gets a new runtime: memory cap, stack cap and a wall-clock
  interrupt (defaults 16 MiB, 512 KiB, 500 ms). Module-level variables never
  survive a call, so `ctx.state` is the only memory.
- **The bridge records; the host acts afterwards.** `ctx.wake` and
  `ctx.state.set` only take effect if `onEvent` returns cleanly. State writes
  commit in one SQLite transaction, so a throw or timeout leaves state as it
  was. A script that fails drops its event (fail closed: it was put there to
  keep noise out).
- **Trust only goes up.** `ctx.wake({ untrusted })` can mark a trusted event
  untrusted, never the reverse.
- **Notes ride the next passed event.** A filter runs before any session
  exists, so a failed reload or failed call becomes a `WakeNote` on the next
  event that script passes (`WakeEvent.notes`, additive and never shown to the
  model), journaled as `Event::Note { class: "script" }` in the session it
  reaches. It is also printed to stderr when it happens. A script that never
  loaded has no next event, so its failure is on stderr only.
- **Operator directory only.** `$CONSTRUCT_HOME/scripts/wakes/` is read;
  a project's `.construct/scripts/` is not, because the agent can write
  there. Reading it waits for the approval flow (#421).
- Limits are crate constants for now; a `[script]` config section comes when
  a second role needs different limits.
