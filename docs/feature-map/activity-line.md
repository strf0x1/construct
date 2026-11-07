---
id: activity-line
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/activity.rs, crates/construct/src/tui/turn.rs, crates/construct/src/tui/theme.rs, crates/construct/src/tui/view.rs]
verified_at: d3ebadf
verified_deps: f95f7499e17e96a2
---
# Activity line

While a turn runs, the line above the prompt says what it is doing — `starting`, `routing`, `waiting for model`, `thinking`, `writing`, `preparing <tool>` or `running <tool>` — with a spinner, the turn's elapsed time, and, while writing, chars/s. Between Enter and the turn's first event the line reads `starting` with no timer; when no turn runs the line is empty.

## Sub-features

- `activity-starting` between Enter and the turn's first event the label is `starting` and no elapsed time is shown.
- `activity-phases` `thinking`, `writing` and `running <tool>` label the phase as the turn moves through it.
- `activity-timer` the elapsed time counts from the turn's start; a queued turn shows none until it starts.
- `activity-chars` while writing, chars/s is shown once the turn has run for half a second.

## How to get to it (user POV)

- Watch the line above the prompt while a turn runs; no key or command is needed.

## Done when

- The first label after Enter is `starting`, with no timer.
- `running <tool>`, `thinking` and `writing` each appear with the timer, and `writing` carries chars/s.
- When the turn ends the line is empty again.

## Driving it

Preconditions:

- Baseline state. The scripted model answers one short reply, then calls a shell tool, then streams a delayed reply. Recipe: `.agents/skills/verify-construct/recipes/activity-line`; its `setup.sh` writes a local-model config and `serve.args` slows the stream so the writing phase lasts.

- **Starting.** Drive: `type first`, `key enter`, then five `snap --style starting-N` in a row. Expect: one snap holds `⠋ starting  esc to interrupt` and no `N.Ns` timer.
- **Tool, thinking, writing.** Drive: `type phases`, `key enter`, `wait "running shell"`, `snap --style tool`, `wait "thinking"`, `snap --style thinking`, `wait "writing"`, `snap --style writing`. Expect: each label on the activity line with the timer, and `chars/s` while writing.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/activity-line`. Expect: `ok: starting has no timer; tool, thinking and writing keep the timer; writing shows chars/s`.

## Gotchas

- The `starting` frame lasts only until the first event (~20 ms at startup), so the recipe snaps several times; a single snap races it.
- `routing` and `waiting for model` last well under a millisecond with the local router and a loopback mock; the recipe does not assert them.
- The `no output for Ns` warning after six quiet seconds is not driven here.
