---
id: delegate-progress
status: verified
verified_at: 066a40d
surfaces: [tui]
depends: [crates/construct/src/tui/children.rs, crates/construct/src/tui/dock.rs, crates/construct/src/tui/jobs.rs, crates/construct/src/tools/call.rs, crates/construct/src/journal/mod.rs]
---
# Delegate progress

While a delegated child works, a row for it sits between the transcript and the prompt: the dock. It reads `⠧ j1  scout  shell sleep 6 · "words so far"   4s  ctrl+t peek`: the job id (`·` for a foreground child), the agent, its last tool and the last line it said, and how long it has run. It is on screen however far the transcript is scrolled, takes no rows when nothing runs, and is gone when the child ends. `ctrl+t` opens the peek panel over the transcript: a bordered popup with the child's last steps (`✓ shell sleep 4 4.0s`, `✗` when one failed), the words it is saying, a tab per child when several run, and a scrollbar once the log is longer than the panel. `esc` closes it. Nothing here is journaled as such: the child's own rows are, under its own session (ADR 0026).

## Sub-features

- `delegate-progress-dock` the row per running child (three at most, then `+N more`), foreground and background.
- `delegate-progress-peek` `ctrl+t`: the panel, `←`/`→` between children, `↑`/`↓`/`pgup`/`pgdn` through the log, `esc` or `ctrl+t` to close; with nothing running `ctrl+t` says `no agents running`.

## How to get to it (user POV)

- Define an agent (`[agents.scout]`), then ask construct to delegate to it, or to delegate in the background ("scout this in the background and tell me what it finds"). The dock appears by itself; press `ctrl+t` to look closer.

## Done when

- While a foreground child works, the dock shows its current tool as `shell sleep 4` with `ctrl+t peek`, in the faint color.
- `ctrl+t` shows the panel with `esc close`; a finished tool is `✓ shell sleep 4` with its time.
- A background child's row stays with the prompt idle (`1 job running` in the status line), also after scrolling the transcript up (`scrolled` in the status line), and is gone after `◆ job j1 completed`.
- Every command shown is a `tool_start` row of the child's own session, and the job's `call_id` is the delegate call.

## Driving it

Preconditions:

- `[agents.scout]` with `tools = ["shell"]`; the mock streams at 500 ms a word (`serve.args`) so the child's words are on screen long enough to snap. Model: a foreground delegate whose child runs `sleep 4` then speaks, a background delegate whose child runs `sleep 6` then speaks, with the parent's `sleep 2` keeping the order of requests. Recipe: `.agents/skills/verify-construct/recipes/delegate-progress`.

- **Foreground dock.** Drive: `type send the scout`, `key enter`, `wait "shell sleep 4"`, `snap --style foreground-dock`, `expect-style "ctrl+t peek" fg=#464c66`, `image {evidence}/foreground-dock.png`. Expect: the row above the activity line.
- **Foreground peek.** Drive: `wait "· found"`, `key ctrl-t`, `expect "✓ shell sleep 4"`, `image {evidence}/foreground-peek.png`, `key esc`, `gone "esc close"`, `wait "foreground done"`, `gone "ctrl+t peek"`. Expect: the panel with the finished tool, and the dock gone with the call.
- **Background, scrolled away.** Drive: `resize 100 18`, `type send another`, `key enter`, `wait "job j1 started"`, `wait "shell sleep 6"`, `expect "1 job running"`, `key pgup`, `expect "scrolled"`, `expect "shell sleep 6"`. Expect: the row stays while the transcript is scrolled up and the prompt idle.
- **Background peek.** Drive: `key esc`, `key ctrl-t`, `expect "j1"`, `image {evidence}/background-peek.png`, `key esc`, `wait "job j1 completed"`, `gone "ctrl+t peek"`, `key ctrl-t`, `expect "no agents running"`. Expect: the panel for the job, the dock gone with the job, and the toast.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/delegate-progress` (runs it after the drive). Expect: `ok: the dock and peek for both children came from the children's own journaled rows; the job is tied to its call` — each command on screen is a child `tool_start` that ended ok, `job_start`'s `call_id` is the delegate call's id, and no `child` row is journaled.

## Gotchas

- The dock redraws once a second while a child runs, and at the usual rate while a turn does; an idle screen with nothing running never ticks.
- The row keeps the last tool until the next one starts; a new tool call clears the words. Reasoning is not shown. The panel keeps the last 50 steps.
- While the panel is open it takes every key except `ctrl+c` and `ctrl+d`, so `esc` closes it rather than interrupting the turn. A permission question hides the dock and the panel until it is answered.
- The scripted model cannot put words and a tool call in one response, so a snap sees the tool first and the words after it.
- Shell jobs have no row (no child); the status line's `◆ N jobs running` counts them.
- Unit tests: `cargo test -p construct --lib tui::children tui::dock`.
