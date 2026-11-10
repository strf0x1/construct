---
id: interrupt-quit
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/input.rs, crates/construct/src/tui/turn.rs, crates/construct/src/tui/status.rs, crates/construct/src/agent.rs, crates/construct/src/tui/mod.rs]
verified_at: 2fe643b
verified_deps: 8448ea7e967a2ca1
---
# Interrupt and quit

`esc` or ctrl-c during a turn interrupts it: the notice reads `■ interrupted after <seconds>`, the running tool is marked failed, and the prompt returns to `ask construct anything… (/help)`. With no turn running, ctrl-c arms quit for two seconds — the status line shows `press ctrl+c again to quit` — and a second ctrl-c exits.

## Sub-features

- `interrupt-esc` esc during a turn cancels it and shows the interrupted notice.
- `interrupt-ctrl-c` ctrl-c during a turn does the same; the journal records `interrupted: true` on the turn.
- `quit-arm` ctrl-c with no turn running arms quit for two seconds; a second press exits, and after the window a press arms again.

## How to get to it (user POV)

- Press esc or ctrl-c while a turn runs.
- Press ctrl-c with an empty prompt and no turn running, then press it again within two seconds to exit.

## Done when

- Each interrupt shows `■ interrupted after <s>` and leaves the prompt idle.
- Two turns interrupted that way are both journaled with `interrupted: true`.
- Idle ctrl-c shows the quit toast; it expires after two seconds, so a later press arms again before a second press exits.

## Driving it

Preconditions:

- Baseline state. The scripted model runs a shell `sleep 8` for each of two turns. Recipe: `.agents/skills/verify-construct/recipes/interrupt-quit`.

- **Esc.** Drive: `type interrupt me`, `key enter`, `wait "running shell"`, `key esc`, `wait "■ interrupted after"`. Expect: the notice and the idle prompt.
- **Ctrl-c.** Drive: a second turn, then `key ctrl-c`. Expect: the same notice; two `turn_end` rows with `"interrupted":true` in `journal.txt`.
- **Quit arm.** Drive: `key ctrl-c` at idle, `wait "press ctrl+c again to quit"`, `sleep 2100`, `key ctrl-c`, `wait "press ctrl+c again to quit"`, `key ctrl-c`. Expect: the toast, an expiry after two seconds, a fresh arm, then exit.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/interrupt-quit`. Expect: `ok: esc and ctrl-c each interrupted a turn; idle ctrl-c armed quit and the second press quit`, with two interrupted `turn_end` rows.

## Gotchas

- The arm lasts two seconds; a ctrl-c after it re-arms instead of quitting.
- A ctrl-c that lands as a turn is ending must arm quit, not interrupt a finished turn: the TUI folds the agent events already received before deciding, and `crates/construct-dev/tests/turn.rs` presses ctrl-c the instant the reply is drawn.
