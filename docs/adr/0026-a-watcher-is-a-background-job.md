# 0026 — A watcher is a background job, not a scheduler

Status: accepted. 2026-10-29.

## Context

#11 asked whether long-running waits, such as watching a PR for CI results
or review comments, need a scheduler with a timer primitive of its own, or are
just background agent definitions (#32) whose completion re-enters through
the journal (#34). ADR 0006 already ruled out the shell's `&`: a shell call's
leftovers die with the call, and anything meant to outlive it "belongs to
background delegation (#34) or a scheduler (#11)".

What other harnesses do, read from source where it is open:

- Claude Code (documented contract, closed source): background commands and
  agents, `Monitor` (each output line an event) and MCP channels all queue
  into one session inbox read at turn boundaries. An idle session starts a
  turn; events that arrive mid-turn are delivered together on the next one,
  in their own tags, not as user text.
- OpenCode (`tool/task.ts`, `effect/runner.ts` at 7985c20): a finished
  background subagent injects a synthetic user message through the normal
  prompt path. One code path, and a cluster of bugs about which user message
  a turn answers (anomalyco/opencode#38523 starvation, #30809 loop exit, #31487
  duplicate streams, #54067 a completion switching the model). Cancelled jobs
  never inject.
- Codex CLI (`tools/parallel.rs`, `session/turn.rs` at 6eab49c): a long
  command returns a session id and the model polls. A process that exits
  while the thread is idle is recorded and never starts a turn, because the
  idle-start path reads only the mailbox (openai/codex#32188, which also
  counts 58 empty poll continuations costing about 6.8M input tokens).
- Hermes Agent (`cron/AGENTS.md` at 088f657): cron jobs are a separate
  system, ticked by a gateway daemon, each run in its own session, because
  they must survive restarts. In-process waits are background processes with
  `notify_on_complete`, and completions, watch matches and async delegations
  share one completion queue.

The options were: (a) a watcher is a background delegate, so a model polls
inside a child; (b) a timer primitive (`wake after N`), so the parent model
polls on each wake; (c) a background command, so a plain process does the
waiting and its exit is the event. (a) and (b) spend a full model request per
poll. (c) spends nothing until something happens, and `gh pr checks --watch`
or `until …; do sleep 60; done` already do the waiting.

## Decision

- There is no scheduler and no timer primitive. A watcher is a background
  job, and a job has two kinds, both on #34's completion queue: a background
  delegate (#34), and a background shell command (`background = true` on the
  shell tool). A delay is a background `sleep`.
- A background command runs under the sandbox helper with the shell's policy
  and permission check at spawn. The helper's lifetime is the job's, not the
  call's: it sweeps the job's process tree when the command exits, is
  cancelled, or construct quits (ADR 0006 otherwise unchanged). Every job has
  a timeout (a new `[tools.shell] background_timeout`, default 30 minutes) and its
  output is capped to a tail. Background commands count against the same
  concurrency limit as background children.
- The call returns a handle at once. Its text tells the model it will be told
  when the job ends, so it does not poll; `check` reads a job's state and
  tail without waiting.
- The wake sources are an explicit list, kept as an enum in one place: a job
  ended on its own (`completed`, `failed`, `timeout`). A job cancelled by the
  model, the user or quitting is journaled and never wakes. Nothing else
  wakes the agent: no webhooks, channels or output-line events.
- A completion is its own `Event` variant, journaled under the parent session
  when the job ends, never a `User` row. The model sees it as a tagged
  message (`<job id=… status=…>`), so a turn always knows whether it answers
  the user or a job.
- Delivery happens only at a turn boundary, never mid-turn (which also keeps
  the prompt cache). Idle in the TUI, a completion starts a turn. Mid-turn,
  completions wait and are delivered together at the start of the next turn.
  A user message typed meanwhile goes first, and the completions ride along
  with it.
- Exactly once: delivery is journaled with the job ids it consumed, and the
  journal decides what is unconsumed. A job whose result the model already
  read with `check` after it ended does not wake again.
- `construct run` waits for its outstanding jobs, bounded by their timeouts,
  and gives each batch of completions a turn before it exits; Ctrl-C cancels
  them.

## Consequences

Easy: watching CI is one shell call that costs no tokens until it ends, and
the same queue, `Event` and delivery rule serve delegates and commands. The
list of wake sources is short and reviewable, which is the omission behind
Codex #32188.

Hard: a job cannot wake the agent on a line of output before it exits (a
review comment while CI still runs). The model has to write the watch so that
it exits on the thing it cares about. A burst of completions during a long
turn costs one turn, not one each, but waits for that turn to end.

Ruled out: anything that outlives construct. Like #34, a job dies with the
process, because the parent is the sole journal writer. A recurring or
restart-surviving job is a system timer running `construct run`, not
something construct schedules (Hermes needs a daemon for this; construct has
none). Also ruled out: timeouts that background a foreground command, and
OpenCode-style injection as a user message.

Revisit when a watcher needs to wake before it exits (a `wake_on` pattern on
the same job, Claude Code's `Monitor`), or when outside events must arrive
without a polling command (an MCP notification as a second wake source,
gated on sender, since an ungated one is a prompt-injection path).

## Evidence

#11, #34, ADR 0006, ADR 0014. Prior art at the commits named above: Codex
`codex-rs/core/src/tools/parallel.rs` and `session/turn.rs` @ 6eab49c,
openai/codex#32188; OpenCode `packages/opencode/src/tool/task.ts` and
`src/effect/runner.ts` @ 7985c20 and the issues cited; Hermes Agent
`cron/AGENTS.md` and `tools/process_registry_notifications.py` @ 088f657;
Claude Code's "How the agent loop works", tools reference and channels
reference (code.claude.com, read 2026-10-20). Nothing here is measured yet.
The token cost of polling is Codex #32188's figure, not ours.

Implemented for background commands in #34's first slice: `crates/construct/src/jobs/`,
`agent/wake.rs`, e2e in `crates/construct-dev/tests/jobs.rs`, TUI recipe
`background-job`. The shell tool's schema grew by about 200 bytes on every
request (the `background` flag and one sentence); the golden request snapshots
were re-recorded for it.

Background delegates are the second slice: `delegate` takes `background = true`
and the child runs as a job (`crates/construct/src/tools/delegate/background.rs`).
It owns its own cancel switch, so interrupting the parent's turn leaves it
running while `job cancel` and quitting stop it; a child that times out ends the
job as `timeout`, one that is interrupted as `cancelled`, and its capped summary
and the pointer to its journal session are the completion's tail. Background
children count against `[jobs] limit` and against a per-definition
`[agents.<name>] max_background` (default 2, 0 turns it off), both refused with
a `job_limit` error. Inside a child the call is refused like any delegation
(depth stays one). The delegate schema grew by one flag and one clause, but
only a session that has agents sees it, so no golden snapshot moved. E2E in
`jobs.rs`; TUI recipe `background-delegate` proves an interrupt keeps the child
running and quitting cancels it.
