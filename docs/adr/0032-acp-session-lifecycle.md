# 0032 — ACP session load, resume and close

Status: accepted. 2026-11-01. Implements #153. Live attachment and the
already-loaded refusal after initialize are superseded by ADR 0033.

## Context

`construct acp` advertised `loadSession: false`, and `session/new` always built
a fresh agent. If the ACP process died, its session could not be reached over
ACP again, even though the journal held everything needed (ADR 0029). Deck
needs to reattach to a task after its agent restarts without replaying ACP
traffic itself (strf0x1/deck#7).

## Decision

The user-mode server advertises `loadSession` and
`sessionCapabilities: { resume: {}, close: {} }`. A delegated child
(`construct __child --acp`) advertises none of them and answers them as
unknown methods: loading there would build a user-config agent inside the
child's boundary.

`session/load` and `session/resume` both build the agent through
`Agent::new(..., Some(id))`, the same path `--resume` uses. Ownership, journal
recovery, the exact-directory check and revalidation of the saved model are
therefore decided once, and the two entry points cannot disagree. The
journal's session id is the ACP session id, so no mapping exists.

`session/load` replays the recorded conversation as `session/update`
notifications before it answers: `submitted` prompts as `user_message_chunk`,
assistant text as `agent_message_chunk`, each tool call as `tool_call` with
its final status, and the `Recovered` note. Compaction is not replayed because
the client did not advertise `session.compaction`. The `Recovered` note is
replayed as an agent message chunk, not as an ACP `notice`: notices are
live-only and require a client capability. A call with no recorded outcome is
`failed`, never `pending`; recovery has already told the model the outcome was
unknown, and nothing is retried. `session/resume` replays nothing.

`session/close` cancels the session as `session/cancel` does, drops its agent
and releases the ownership lock. It is idempotent, and an unknown session
still succeeds. A close mid-turn answers before the turn task has dropped its
handle, so the kernel lock is released when the cancelled turn ends, not when
the response is sent.

Refusals name the reason: an unknown id is ACP `-32002` with
`data.sessionId`; a session from another `cwd`, a session already loaded on
this connection, and a session owned by another process are `-32602`.

The client's `mcpServers` are ignored, as `session/new` ignores them:
construct's MCP servers come from config, and their environment and headers
never reach a journal, an error or a replayed update. A loaded user session
installs the same `ClientAsk` permission asker as `session/new` (ADR 0016).

## Consequences

A client can reattach after a crash, a stopped VM or a host reboot, and one
journal still has one writer at a time. Reattaching to a turn that is still
running in a live process remains the host runtime's job; this is conversation
restoration, not process or provider-cache restoration. A client that loads
must handle a tool call whose external effect is unknown exactly as the model
must: inspect before retrying.

## Evidence

Issue #153. `crates/construct-dev/tests/acp.rs` drives the real binary:
capabilities and each refusal, a session killed mid-tool and loaded by a new
process (history replayed, the interrupted call failed and not re-run, the
session prompting again), resume with no history, and close mid-turn followed
by a second close. `acp::map` pins the replay projection.
