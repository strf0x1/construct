---
id: background-jobs
status: verified
verified_at: 4401039
surfaces: [tui, cli]
depends: [crates/construct/src/jobs, crates/construct/src/cli/headless.rs, crates/construct/src/agent/wake.rs, crates/construct/src/tools/shell_job.rs, crates/construct/src/tools/job.rs, crates/construct/src/tools/delegate/background.rs, crates/construct/src/tui/jobs.rs, crates/construct-dev/tests/jobs.rs]
---
# Background jobs

The model can run a shell command, or a delegated agent, in the background (`background = true`) and carry on. The call returns a job id at once; when the job ends on its own (completed, failed, or timed out) the agent is told, with the end of its output (for an agent, its summary), and takes a turn on it even though the user typed nothing. A job the model or the user cancels, or that quitting stops, is journaled and never wakes the agent. The transcript shows `◆ job j1 started` and `◆ job j1 completed · 2.0s`, and the status line shows `◆ N jobs running` while any run. This is how construct watches CI or waits out a delay: there is no scheduler, only a command that exits on what it waits for (ADR 0026).

## Sub-features

- `background-jobs-wake` an idle TUI starts a turn when a job ends; `construct run` waits for outstanding jobs and gives each batch of endings a turn before it exits.
- `background-jobs-indicator` the transcript lines and the status-line count.
- `background-jobs-check` the `job` tool reads a job's state and output tail without waiting, and a result read this way does not wake the agent again.
- `background-jobs-cancel` `job cancel` stops a job; it is journaled as `cancelled` and wakes nobody.
- `background-jobs-delegate` `delegate` with `background = true` starts the child as a job; its capped summary and a pointer to its journal session are the completion. It has a cancel switch of its own, so interrupting the parent's turn leaves it running, while `job cancel` and quitting stop it.
- `background-jobs-limits` `[tools.shell] background_timeout` (default 30 minutes) ends a command as `timeout`; an agent's own `timeout` ends a child as `timeout`; `[jobs] limit` (default 4) and `[agents.<name>] max_background` (default 2, 0 is off) refuse a job past them with `job_limit`.

## How to get to it (user POV)

- Ask construct to run something long in the background and tell you when it finishes, e.g. "run the tests in the background". Nothing else to type: when the job ends the reply appears by itself.
- `construct run "<prompt>"` does the same without a terminal; it stays until the jobs it started have ended and been answered.

## Done when

- The call's result is a handle (`started job j1 …`) and the turn ends before the command does.
- When the command ends, a second turn starts with no user input, and the model's request holds a `<job id="j1" … status="completed">` message with the command's output.
- The journal has `job_start`, `job_end`, and `jobs_delivered` (with `"via":"turn"`) in that order, two `turn_start` rows, and no `user` row for the completion.
- A cancelled job has a `job_end` row with `status: cancelled` and no `jobs_delivered` row.
- The status line says `1 job running` while it runs and the count is gone after.

## Driving it

Preconditions:

- Baseline state. Model: a background `sleep 2 && echo watched-done`, then `started, waiting`, then `it said watched-done`. Recipe: `.agents/skills/verify-construct/recipes/background-job`.

- **Start.** Drive: `type watch it`, `key enter`, `wait "started, waiting"`, `wait "job j1 started"`. Expect: the shell block shows the handle and the turn has ended.
- **Indicator.** Drive: `snap --style running`, `expect "1 job running"`, `image {evidence}/running.png`. Expect: the status line carries the count while the job runs.
- **Wake.** Drive: `wait "job j1 completed" 6000`, `wait "it said watched-done" 8000`, `gone "1 job running" 500`. Expect: the reply arrives with nothing typed, and the count is gone.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/background-job` (runs it after the drive). Expect: `ok: completion delivered once as jobs_delivered; two turns, one user row` — the last request holds the tagged completion and `watched-done`, and `journal.txt` has two `turn_start` rows, one `user` row and one `jobs_delivered` row.

### Background delegate

Preconditions:

- `[agents.scout]` with `tools = ["shell"]`. Model: a delegate with `background = true` and a `sleep 1`, the child's `sleep 4`, a parent `sleep 120`, the child's summary `scout-done: found nothing`, `the scout found nothing`, then a second background delegate. Recipe: `.agents/skills/verify-construct/recipes/background-delegate`.

- **Interrupt.** Drive: `type send the scout`, `key enter`, `wait "job j1 started"`, `wait "sleep 120"`, `key esc`, `expect "1 job running"`. Expect: the turn is interrupted and the child is still counted.
- **Wake.** Drive: `wait "job j1 completed" 10000`, `wait "the scout found nothing"`, `gone "1 job running"`. Expect: the summary starts a turn with nothing typed.
- **Quit.** Drive: `type send another`, `key enter`, `wait "job j2 started"`, `key ctrl-d`. Expect: the process exits and the journal has j2 as `cancelled`.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/background-delegate` (runs it after the drive). Expect: `ok: interrupt left the child running; its summary woke the parent; quitting cancelled the second child` — j1's `job_end` comes after the parent's interrupted `turn_end`, there is exactly one `jobs_delivered` row, and the wake request holds the child's summary.

## Gotchas

- The cancel, check, timeout and limit sub-features are proven by `cargo test -p construct-dev --test it jobs::` (the real binary under `construct run`, asserting requests and journal rows), not by the TUI recipe.
- A background child is a child process like a foreground one: depth stays one, so a child cannot start one, and its questions for the user (permission prompts) are asked as the job runs, whatever the parent is doing.
- `construct run`: Ctrl-C during a turn interrupts that turn (journaled as interrupted), cancels the jobs and exits; it no longer kills the process mid-turn. Proven by `cargo test -p construct-dev --test it jobs::ctrl_c`.
- A background child's permission questions are rejected where nobody can be asked (`construct run`), so it gets a refusal and the job still ends on its own (`jobs::a_background_child_reaching`).
- A job dies with construct: quitting cancels it (journaled). Nothing survives a restart.
- A completion that arrives while a turn runs waits for the next turn boundary; the recipe's job ends while the TUI is idle.
