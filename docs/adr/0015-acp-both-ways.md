# 0015 — ACP, both ways

Status: accepted. 2026-10-22.

## Context

#33: editors and other harnesses speak the Agent Client Protocol, and the
agents worth delegating to (codex, claude, gemini, opencode) have ACP adapters.
ADR 0014 made delegation a child process on construct's own JSON lines and
said ACP would replace the wire and keep the rest.

## Decision

- `construct acp` serves construct as an ACP agent (`acp/server.rs`):
  `initialize`, `session/new`, `session/prompt` (streamed `session/update`s,
  then a stop reason), `session/cancel`. The ACP session id is the journal
  session id. construct has no approval flow (the sandbox is the boundary), so
  the server never sends `session/request_permission`.
- A definition with `protocol = "acp"` and no `command` runs
  `construct __child --acp`; with `command = [...]` it runs that program. Both
  run under the same sandbox helper, environment scrub and timeout as a JSONL
  child. `child/run.rs` owns the process; a `Wire` (`child/wire.rs`) is only a
  state machine over lines, so a protocol cannot leave a child running.
  The JSONL transport stays: it is the test transport and what `--json`
  readers know.
- ACP is lossy for construct's record, so a construct child gets a lossless
  side channel: a client that sets `clientCapabilities._meta.construct.events`
  also receives every `Event` as an `_construct/event` notification instead of
  the standard updates. The parent journals those, so construct over ACP
  writes the same rows as construct over JSONL. Any other agent is journaled
  from its standard updates: a synthetic head (harness is the definition's
  name), the task, text, tool calls and results, and the end of the turn.
  Real adapters open a call as `pending` with partial input and send the real
  input when it starts, so `tool_start` is journaled at the first
  non-pending update, not at the first message. Token use comes from the
  prompt response's `usage` when the agent sends one.
- `session/new`'s `_meta.construct.start` (config and origin, as JSONL's
  `start`) is honoured only by `__child --acp`, never by `construct acp`, so
  an ACP client cannot widen what the user configured.
- `session/request_permission` is answered by the definition's
  `on_permission` (`reject` by default, `allow`), choosing only an option of
  the wanted kind, else `cancelled`; never the opposite kind. The request and
  answer are journaled (`Event::Permission`). Anything else the child asks of
  the client (files, terminals) is refused as an unknown method: construct
  advertises none.
- `session/update` tool calls carry ACP's fixed `kind` (`read`, `edit`,
  `search`, `execute`, `fetch`, `other`), declared once per tool as
  `Tool::kind`; the name stays in `title`.
- `[agents.x] env = { NAME = "value" }` sets plain variables for the child
  (`CODEX_HOME`, a model choice); credentials stay in `secrets`, and the
  names construct sets itself (`CONSTRUCT_HOME`, `CONSTRUCT_DEPTH`, `TMPDIR`)
  are refused.
- A `command` agent runs its own tools, prompt and model, so `tools`,
  `prompt`, `model` and `base_url` are refused with it, and it gets no
  implicit endpoint key. It needs `allow_read` for its install path and
  `network = true` for its own provider.

## Consequences

Easy: any ACP agent is a config table. Hard: containment of a foreign agent is
the process boundary only (its tool set is not ours to narrow), and nothing
asks a human yet: a TUI prompt for `request_permission` is a follow-up, as is
`diff` content for edit permission requests (Claude's adapter sends one).

## Evidence

#33. A live parent (z-ai/glm-5.3-flash on OpenRouter) chose to delegate to a
construct scout over `jsonl` and over `acp`: same answer, same journal rows
per session. Run for real against opencode 1.18 (`opencode acp`, a free model) as a
sandboxed child: handshake, a prompt, `bash` tool calls, journaled under its
ACP session id. That run found that `network = true` children never started
(a port rule for TCP the ruleset didn't handle); fixed in `sandbox/mod.rs` with
a regression test. Run again against `claude-agent-acp` 0.94 (a real
`session/request_permission`, answered `reject` and `allow`: the file was
written only on `allow`) and Esc in the TUI mid-delegation (opencode aborted
its running tool; nothing was left behind). Those runs also found that
`codex-acp` ignores stdin closing after its turn, so a finished child is
stopped after a grace period instead of waited for, and that a tool's `name`
(not its display `title`) is what the record calls it. `codex-acp` 0.16
itself could not complete a turn on a ChatGPT login (every model it offers is
rejected for that account), so its permission path is untested.
`crates/construct-dev/tests/acp.rs` (conformance smoke, lossless stream,
cancel) and `delegate.rs` (construct over ACP matches JSONL; a scripted
foreign agent from `construct-dev acp-fake`: permission both ways, unknown
methods, cancel on timeout).
