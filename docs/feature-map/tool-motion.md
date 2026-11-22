---
id: tool-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion.rs, crates/construct-tui/src/view.rs, crates/construct-tui/src/app_events.rs, crates/construct-tui/src/lib.rs]
verified_at: c925454
verified_deps: 6a21ad7dfa4a0477
---
# Motion: comet and unfurl

While a turn runs, a comet (a bright head with a fading tail) laps the rule above the input, in the color of what the turn is doing: lavender thinking, yellow running a tool, blue waiting or writing, cyan routing. It is the activity line's highlight color, so the two agree. When the turn ends the comet docks with one soft flash and the rule is plain again. When a tool finishes, its header flashes its outcome (pale green ✓, pale red ✗) and its output unfurls: the `│` gutter fills top to bottom with a bright head while the text trickles in from the top. `CONSTRUCT_MOTION=off` turns all of it off and draws exactly the old screen (ADR 0064).

## Sub-features

- `motion-comet` a comet laps the rule while a turn runs, colored by the phase.
- `motion-dock` idle, the rule is plain faint `─` again after one flash.
- `motion-unfurl` a finished tool's gutter fills with a bright `╽`/`┃` head and its text coalesces top-down, then settles to the plain preview.
- `motion-off` `CONSTRUCT_MOTION=off` (or `0`, `false`) disables motion; tests and recipes run with it off unless a recipe's `drive.env` turns it on.

## How to get to it (user POV)

- Send any prompt that runs a tool and watch the rule above the input and the tool's output; no key or command is needed.

## Done when

- While the tool runs, the rule carries one white `━` head with a colored tail.
- Just after the tool finishes, its gutter shows the bright `╽`/`┃` head and the rows below it are not yet drawn.
- Idle, the rule has no `━` and the output shows every line with a plain `│`.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/tool-motion`; its `drive.env` sets `CONSTRUCT_MOTION=on`, its `setup.sh` writes a local-router config, and the scripted model runs one shell command that prints three words and sleeps a second, then replies after a delay.

- **Comet.** Drive: `type list three words`, `key enter`, `wait "running bash"`, `snap --style busy`. Expect: the rule row holds `⟦fg=#ebeeff⟧━`.
- **Unfurl.** Drive: `wait "✓ bash"`, then `snap --style unfurl-1` … `unfurl-4`. Expect: one snap holds `╽` or `┃` in the gutter.
- **Idle.** Drive: `wait "ask construct anything… (/help)"`, `sleep 1500`, `snap --style idle`. Expect: no `━`, no `╽`/`┃`, and `gamma-out` under a plain `│`.
- **Second view.** The recipe's `check.sh` also reads the journal: the `tool_end` row carries `gamma-out`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/tool-motion`. Expect: `ok: comet while running, the output unfurled, idle rule plain, tool output in the journal`.

## Gotchas

- The unfurl lasts ~700 ms, so the recipe snaps four times in a row; a single snap can race it.
- Match the box-drawing glyphs with `grep -e '╽' -e '┃'`, not a bracket class: in a byte locale `[╽┃]` matches bytes of the plain `│` too.
- Bash output reaches the TUI only when the tool ends (there is no live output event yet), so the unfurl plays then; a live tail while running would need one.
- The comet is not drawn while a permission panel is up: the panel takes the rule's place.
