# construct

A lean, single-binary coding agent in Rust. One idea, really:

> **Hermes Agent's self-improving loop, at opencode's speed.**

Hermes gets better the more you use it — it watches a session, and when a
problem keeps happening it writes itself a skill or a memory so it doesn't
trip on it again. opencode is fast: first frame is instant, the first request
is small, nothing blocks the UI. construct is an attempt to have both in one
binary you can drop on a box: an OpenAI-compatible streaming loop, in-process
tools, capped memory and agent-written skills, a background learning pass, a
Jev/local router, and a zero-flicker TUI.

## Why this exists

This started as a goal-style challenge — *how far can a strong model push a
harness if you just point it at the problem and let it iterate?* — and it's
meant to stay fun. It's not a product and it's not a résumé line. It's a thing
we build because building it is the good part.

But it has one serious rule, and the rule is the whole point:

> **Measure, change, measure again. Post every number — including the losses.**

No "feels faster." No shipping a change because it's clever. If a change can't
show up in a benchmark, it doesn't land, and if a benchmark says we got worse,
that goes in the log too. The entire history of what we tried, what won, and
what embarrassed us lives in [`bench/results/logs/observations.md`](bench/results/logs/observations.md).
Read that first if you want to know what actually happened versus what sounded
good at the time.

## Why Rust

Fast and safe, and here both matter literally. Fast: the harness overhead
(first frame, first request size, tool latency, memory) has to disappear next
to the model's own latency, and Rust lets us get there without a runtime
fighting us. Safe: the agent runs real shell commands and touches the
filesystem, so the boundary around it is load-bearing. On Linux that boundary
is Landlock, wired straight into the binary (`crates/construct/src/sandbox/mod.rs`). An agent with
nothing to go on *searches the disk* — we've watched one read the API-key files
and another hand-roll a SHA-256 brute-forcer — and the sandbox is what turns
that from an incident into a contained, denied, logged non-event.

## The shape of it

| Piece | Where | What |
|---|---|---|
| The loop | `crates/construct/src/agent.rs`, `crates/construct/src/llm.rs` | Route, stream, run tools, compact. Everything observable is an `Event`. |
| Tools | `crates/construct/src/tools/` | shell, read/write/edit/grep/glob, web, memory/skill/session search. |
| Router | `crates/construct/src/router.rs`, `crates/construct/src/jev.rs` | Per-turn tool groups + skill suggestions. Jev (TypeSafe's System One) when a key resolves, local otherwise. |
| Learning | `crates/construct/src/learn.rs` | Observation ledger → recurrence gate → a skill or memory only when a problem *repeats*. Runs detached after a session. |
| Memory & skills | `crates/construct/src/memory.rs`, `crates/construct/src/skills.rs`, `crates/construct/src/project.rs` | Capped `§` memory, agent-written skills, and per-repo "project kits" that follow the repo, not the session. |
| Sandbox | `crates/construct/src/sandbox/mod.rs` | The boundary: Landlock for shell and MCP servers, path checks for file tools, a credential-free env. |
| TUI | `crates/construct/src/tui/` | ratatui app, first frame waits on nothing. |

For how to actually build, run, and test it — including running the whole thing
against a scripted model with no API key — see **[AGENTS.md](AGENTS.md)**. That's
the working manual; this file is the why.

## What's benchmarked (and what isn't, yet)

Honesty clause, because it's easy to overclaim:

- The microbenchmarks (`size`, `frame`, `tools`, `memory`) measure harness
  overhead directly, against a mock model.
- The behavioral suites (`task`, `routing`, `loop`) run a real model against
  **one tiny synthetic fixture** — a ~6-line Python `stats` library with a
  planted bug, a planted routing landmine, or a planted learning landmine. These
  are sharp instruments for isolating one property each (first-request size,
  routing hit, learning-on-recurrence). They are **not** evidence of edit
  quality on real code. That gap is tracked in issue #15.
- Adapters exist for `hermes` and `opencode` (and their source is vendored in
  `research/src/` for reading how they work), but we have **not** run a
  real-model head-to-head against them yet — that's gated on sandboxing them
  first (issue #4). So "beats Hermes and opencode" is the *goal*, not yet a
  *result*.

The one clean behavioral win so far: the self-improving loop works end to end.
On the seeded loop bench, construct passes the held-out session 3/3 while a
no-memory control passes 0/3, because the learner wrote itself the missing
procedure after seeing the problem twice. Numbers and caveats are in the
observations log.

## Where to pick up

1. Read [`bench/results/logs/observations.md`](bench/results/logs/observations.md) — the real story, including every dead end.
2. Read [AGENTS.md](AGENTS.md) — how to build, run, and test.
3. Skim the open [issues](https://github.com/strf0x1/construct/issues) — that's the queue, roughly in priority order. Most of them came out of a benchmark telling us something.
4. When you make a change: score it on the relevant bench before and after, and add a line to the observations log. If you can't measure it, that's the first problem to solve, not an exception to the rule.

Keep it honest, keep it measured, and keep it fun.
