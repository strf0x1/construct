# 0055 — Delegation is removed

Status: accepted. 2026-11-20. Supersedes 0014 and 0017; partly supersedes
0015 (the client half), 0016 (a child asking its parent) and 0028 (a child's
network-only lock).

## Context

ADR 0014 added `delegate`: the model hands a task to an `[agents.<name>]`
definition, run as a sandboxed `construct __child` process or, per 0015, as
any ACP agent. It earned its keep less than it cost. Children were rarely
worth a turn over doing the work in the session, and every guarantee had to be
made twice: per-call locking for children (0017), an upward permission channel
(0016), per-agent grants and `[agents.*]` persistence, a child's endpoint and
key, a network-only lock that Seatbelt cannot stack (0028), a dock and peek
panel in the TUI, and a cluster of two-way module pairs (`acp <-> child`,
`child <-> tools`, `agents <-> config`, `agents <-> tools`) that the
modularity work (#303) would otherwise have had to untangle (#294).

## Decision

- There is no `delegate` tool, no `[agents.<name>]` table, and no hidden
  `construct __child` subcommand (JSON lines or `--acp`). The `child` and
  `agents` modules, `acp::client`, `permission::Upstream`, `sandbox::Grant`,
  `Policy::for_agent`/`net_only`/`connect_ports`, and the TUI's dock and peek
  panel are gone. A stale `[agents.*]` table in `config.toml` is ignored.
- construct speaks ACP one way only: `construct acp`, as an agent. It does
  not honour `_meta[...].start` (it never did outside `__child --acp`) and
  advertises no client capabilities.
- A permission question is one of two kinds: a blocked path or a developer
  operation. Session grants are the session's own; "always" writes only to
  `[sandbox]`.
- Background jobs are shell commands only (`jobs::Kind::Shell`).
- The journal vocabulary stays additive (ADR 0008): the wire-only
  `Event::Ask` and `Event::Child` are removed, since no journal holds them.
  What old journals do hold still reads: `Head.parent_session` and
  `parent_call_id` (so old child sessions stay out of the browser and
  `sessions show` still says whose child one was), `Event::Permission`,
  `Access.agent`, and `jobs::Kind::Delegate`. Nothing writes them now.

## Consequences

- One sandbox model: the session's policy, widened per call by what the user
  approved. Seatbelt's no-stacking limit no longer matters to construct's own
  process.
- Resuming an old session that delegated no longer adds its children's spend
  to `/status`, and no longer nests their transcripts under the delegate call.
- Deck's subagent story is the coordinator's `deck` tool (separate sessions),
  not children.
- Revisit if a task shape appears where a narrower, separately prompted
  worker clearly beats the session doing the work itself, measured with
  `bench.py task`. Bring it back as a new ADR, not by reverting this one.

## Evidence

- Issue #294 (the decision), #303 (the module-graph inventory: what the
  removal cuts).
- The `delegate-scout` bench task and the `delegate`, `child` e2e files are
  removed with the feature; the `rust-replay-child-key*` tasks replay a
  historical PR at its own commit and are unaffected.
