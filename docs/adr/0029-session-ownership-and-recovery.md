# 0029 — Session ownership and explicit recovery

Status: accepted, 2026-10-31. Implements #127.

## Context

The journal already preserves structured messages and compaction cuts (ADR
0008), but its projector is the effective model context rather than the full
transcript. A session browser also needs original prompts, recognizable names,
activity and writer ownership. Switching an idle conversation must not inherit
permissions or asynchronous output from the previous runtime. A stopped
process can leave tool calls without model-facing results and jobs without a
recorded completion.

## Decision

- Keep the journal as the only conversation store. `submitted` records the
  original prompt before routing, separate from the augmented `user` message;
  `session_name` records a manual name. Browser rows derive from top-level
  heads in the exact directory with at least one original/legacy prompt.
  Names, model choices, learning and recovery do not change activity. `-c`
  deliberately keeps its newest-started rule, even though the browser sorts
  by conversation activity.
- A `Writer` holds an OS advisory file lock for its session, with a filename
  derived from the session ID. The file's existence is not ownership: process
  death releases the kernel lock. Different sessions share SQLite WAL and
  can run concurrently. Browsing opens the journal read-only and probes
  existing locks without creating heads or lock files.
- Restore only while idle. Validate exact directory and saved model/provider
  against current configuration and credentials, acquire ownership and build
  the destination before replacing the active runtime. An explicit `--model`
  or a recovery model choice overrides saved policy. Missing providers never
  fall back to another provider implicitly. Build fresh tools, sandbox,
  permissions and cancellation handles. A readiness acknowledgment and a
  distinct runtime generation fence event delivery, including A → B → A.
- Replay compaction for model context, validating saved boundaries and tool
  pairing. Rebuild the UI from full durable history, including delegated
  history, with normal tool expansion. Restore accounting from the same
  journal. Unknown, malformed or incompatible restoration rows fail clearly;
  general-purpose tolerant journal inspection remains available.
- A recovery transaction fills missing tool results by call ID: use a durable
  `tool_end` result when known, otherwise supply an unknown/interrupted
  result that warns of possible external effects. Preserve an original
  prompt whose routing never finished. Lost background jobs become
  interrupted; committed completions remain pending until explicit user
  continuation. `recovered` closes the unmatched turn boundary. Reopening
  adds no further repairs. Repair timestamps preserve conversation recency.
- Selecting a session never retries a tool or starts generation. This is
  conversation restoration, not process, filesystem or provider-cache
  restoration. Unrecorded streaming tokens may be lost. Journal write
  failures are surfaced as degraded recovery rather than silently ignored.

## Consequences

Two processes cannot resume the same writer concurrently, and no stale lock
file needs manual deletion. The browser needs no credentials or model calls.
Original text is available for names and UI history without exposing injected
skill instructions as user prompts. The archive is complete rather than
silently capped; loading and filtering currently materialize metadata for all
eligible sessions off the UI path. A larger archive may eventually justify an
indexed metadata query or lazy-loading interface.

The current version refuses live handoff and exact-directory mismatches.
Cross-directory browsing, session forks, restoring operating-system processes
and automatically resuming interrupted generation remain separate decisions.
Older journals have no separately recorded original prompt, so their labels
use legacy user text with reminder stripping. History with unreplayable old
compaction is explicitly incompatible with safe restoration.

## Evidence

- Issue: https://github.com/strf0x1/construct/issues/127.
- `construct-dev` scenarios `sessions::`, `recovery::` and `compaction::`
  inspect journal rows and the next recorded model request. Forced process
  termination covers routing, streaming and the gap after one tool outcome
  while another outcome remains unknown.
- Verification recipe `session-browser` captures wide/narrow browser images
  and a restored transcript; its second view checks IDs, names, original
  prompts, accounting rows, isolated models/context and tool pairing.
- The existing help golden snapshot is intentionally re-recorded to include
  `/resume` and `/rename`; model-request snapshots remain unchanged.
