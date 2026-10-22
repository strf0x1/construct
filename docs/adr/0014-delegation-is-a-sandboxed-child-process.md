# 0014 — Delegation is a sandboxed child process

Status: accepted. 2026-10-22.

## Context

Orchestrator-to-worker patterns need a model-facing tool, and something behind
it. The decisions in #29 chose a process over an in-process loop: children use
a different model and prompt by design, so sharing the parent's prompt cache
is no reason for in-process, while a process gives a narrower sandbox, a
credential-free environment, and crash and cancel isolation (#32).

## Decision

- A `delegate` tool exists only when `[agents.<name>]` is configured. It runs
  the definition as `construct __child` under the sandbox helper and returns
  the child's last answer, capped at 8 KB, with the child's session id in
  `ToolEnd.data`. The transcript is in the parent's journal, not the result.
- Containment is subtraction from the parent (`agents::AgentDef::child_config`).
  A child's tools are its allowlist within the parent's catalog; an empty
  allowlist is no tools. Groups `memory`, `skill-write` and `history` are
  refused at session start, never silently dropped. A child has no MCP servers
  and no agent definitions, never learns, and `CONSTRUCT_DEPTH` caps nesting
  at one regardless of definition.
- The child's sandbox (`Policy::for_agent`) lists only what its definition
  grants, each path inside the parent's own policy. The parent's working
  directory is not implied: without a grant the child works in a scratch
  directory. TCP is denied except the model endpoint's port, unless
  `network = true`.
- Credentials follow the endpoint. The child's environment is scrubbed, then
  given only the secrets its definition names plus its endpoint key. A
  definition with its own `base_url` and no `api_key_secret` gets no key, and
  neither the parent's `headers` nor its provider dialect.
- The wire is JSON lines (`child/wire.rs`): `start`, `prompt`, `cancel` in;
  `Event` lines out. Closing stdin cancels a turn in flight and ends the
  child. The parent alone journals: `Call::forward` writes each child event
  under the child's session id (named by its `head`) and sends it up wrapped
  as `Event::Child { id: call id, inner }`, which is wire only. The child's
  own `CONSTRUCT_HOME` is scratch inside its sandbox temp and is removed.
- Lifecycle: a cancel or timeout sends `cancel`, waits a grace period, then
  stops the helper (SIGTERM, then the group); the helper sweeps everything the
  child started, and a dying parent takes the child with it
  (`PR_SET_PDEATHSIG` in the helper, ADR 0006).

## Consequences

Easy: a narrower worker is a config table, and every limit above is observable
from the parent's journal and `--json`. Hard: the child pays process and model
startup per delegation, and its nested children are not journaled by the
parent (depth is capped at one, so none exist). `network = true` widens only
TCP; a child can still not read what its definition does not grant. ACP (#33)
replaces `child/wire.rs` and keeps the rest. Background delegation (#34)
reuses `child::run` with the completion queued in the journal.

## Evidence

#32. End-to-end tests in `crates/construct-dev/tests/delegate.rs` and
`child.rs`; the child's first request is pinned by the `delegated_child`
snapshot.
