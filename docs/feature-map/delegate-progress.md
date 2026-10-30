---
id: delegate-progress
status: built
surfaces: [tui]
depends: [crates/construct/src/tui/nested.rs, crates/construct/src/tui/jobs.rs, crates/construct/src/tools/call.rs, crates/construct/src/journal/mod.rs]
---
# Delegate progress

While a delegated child works, the transcript shows what it is doing under the call that started it: its current tool call (`↳ shell sleep 4`) and the last line it has said (`↳ found the notes and am …`), two faint lines at most. A foreground child's lines sit under the running `delegate` call and go when the call ends. A background child's sit under its `◆ job j1 started` line, stay while the TUI is idle, and go when the job ends. Nothing is drawn for a child that has done nothing yet, and the lines are never journaled as such: the child's own rows are, under its own session (ADR 0026).

## Sub-features

- `delegate-progress-foreground` lines under the running `delegate` call, gone when it ends.
- `delegate-progress-background` lines under the job's start line, kept after the delegate call has returned its handle, gone when the job ends.

## How to get to it (user POV)

- Define an agent (`[agents.scout]`), then ask construct to delegate to it, or to delegate in the background ("scout this in the background and tell me what it finds"). The lines appear by themselves.

## Done when

- A foreground child's current tool shows as `↳ <tool> <argument>` under the running `delegate` call, in the faint color, and its words follow as they stream.
- After the call ends, no `↳` line is left on screen.
- A background child's lines show under `◆ job j1 started`, with `1 job running` in the status line and the prompt idle, and are gone after `◆ job j1 completed`.
- Every command shown on screen is a `tool_start` row of the child's own session, and the job's `call_id` is the delegate call it was filed under.

## Driving it

Preconditions:

- `[agents.scout]` with `tools = ["shell"]`; the mock streams at 500 ms a word (`serve.args`) so the child's words are on screen long enough to snap. Model: a foreground delegate whose child runs `sleep 4` then speaks, a background delegate whose child runs `sleep 6` then speaks, with the parent's `sleep 2` keeping the order of requests. Recipe: `.agents/skills/verify-construct/recipes/delegate-progress`.

- **Foreground tool.** Drive: `type send the scout`, `key enter`, `wait "↳ shell sleep 4"`, `snap --style foreground-tool`, `expect-style "↳ shell sleep 4" fg=#464c66`, `image {evidence}/foreground-tool.png`. Expect: the faint line under the running `delegate scout`.
- **Foreground words.** Drive: `wait "↳ found"`, `wait "foreground done"`, `gone "↳ "`. Expect: the words appear under the tool line, and everything is gone with the call.
- **Background.** Drive: `type send another`, `key enter`, `wait "job j1 started"`, `wait "↳ shell sleep 6"`, `snap --style background-tool`, `expect "1 job running"`, `wait "↳ the second"`, `wait "job j1 completed"`, `gone "↳ "`. Expect: the lines sit under the job's line while the prompt is idle, and go when the job ends.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/delegate-progress` (runs it after the drive). Expect: `ok: the lines under both delegate calls came from the children's own journaled rows; the job is tied to its call` — each command on screen is a child `tool_start` that ended ok, `job_start`'s `call_id` is the delegate call's id, and no `child` row is journaled.

## Gotchas

- Only the last tool is kept between tools, and a new tool call clears the words: the lines show what the child is doing now, not a log. Reasoning is not shown.
- A background child's lines stay where its job line is. After more turns they scroll up with it; the status line's `◆ N jobs running` is the always-visible part.
- The scripted model cannot put words and a tool call in one response, so a snap sees the tool line, then the words after it; a real model's text before a tool call is shown for as long as the model takes to stream the call.
- Lines are cut at 70 columns; on a narrower terminal they wrap.
- Unit tests: `cargo test -p construct --lib tui::nested`.
