---
id: tool-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/grow.rs, crates/construct-tui/src/motion/ink.rs, crates/construct-tui/src/motion/rule.rs, crates/construct-tui/src/motion/shaders.rs, crates/construct-tui/src/view.rs, crates/construct-tui/src/app_events.rs, crates/construct-tui/src/lib.rs]
verified_at: 0fbd4ad
verified_deps: 7cf4cf20ce96adcd
---
# Motion: comet, unfurl and drying text

While a turn runs, a comet (a bright head with a fading tail) laps the rule above the input, in the color of what the turn is doing: lavender thinking, yellow running a tool, blue waiting or writing, cyan routing. It is the activity line's highlight color, so the two agree. When the turn ends the comet docks with one soft flash and the rule is plain again. When a tool finishes, its header flashes its outcome (pale green ✓, pale red ✗) and its output unfurls: the block opens row by row over half a second, pushing the transcript up, while the `│` gutter fills top to bottom with a bright head and the text trickles in from the top. The reply that follows dries as it streams: its newest characters land bright and cool to their own color behind a blinking `▍` caret. `CONSTRUCT_MOTION=off` turns all of it off and draws exactly the old screen (ADR 0065, 0066).

## Sub-features

- `motion-comet` a comet laps the rule while a turn runs, colored by the phase.
- `motion-dock` idle, the rule is plain faint `─` again after one flash.
- `motion-unfurl` a finished tool's output opens row by row from its header down, its gutter fills with a bright `╽`/`┃` head and its text coalesces top-down, then settles to the plain preview.
- `motion-ink` a streaming reply's tail is bright and dries to ink, with a `▍` caret after it while the model writes.
- `motion-off` `CONSTRUCT_MOTION=off` (or `0`, `false`) disables motion; tests and recipes run with it off unless a recipe's `drive.env` turns it on.

## How to get to it (user POV)

- Send any prompt that runs a tool and watch the rule above the input and the tool's output; no key or command is needed.

## Done when

- While the tool runs, the rule carries one white `━` head with a colored tail.
- Just after the tool finishes, its gutter shows the bright `╽`/`┃` head, and the block has fewer rows under its header than it settles at.
- While the reply streams, its line ends in `▍` with the characters before it bright (red and green channels `d0` or more).
- Idle, the rule has no `━`, the output shows every line with a plain `│`, and there is no `▍`.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/tool-motion`; its `drive.env` sets `CONSTRUCT_MOTION=on`, its `setup.sh` writes a local-router config, and the scripted model runs one shell command that prints eight words and sleeps a second, then replies after a delay, one word every 90 ms (`serve.args`: `--chunk-ms 90`).

- **Comet.** Drive: `type list eight words`, `key enter`, `wait "running bash"`, `snap --style busy`. Expect: the rule row holds `⟦fg=#ebeeff⟧━`.
- **Unfurl.** Drive: `wait "✓ bash"`, then `snap --style unfurl-1` … `unfurl-4`, `sleep 600`, `snap --style opened`. Expect: one snap holds `╽` or `┃` in the gutter, and one has fewer rows between `✓ bash` and the activity line than `opened` (five).
- **Ink.** Drive: `wait "listed eight"`, `snap --style stream-1` … `stream-4` 120 ms apart. Expect: one snap's `▍` line has a bright `fg` on the characters before it.
- **Idle.** Drive: `wait "ask construct anything… (/help)"`, `sleep 1500`, `snap --style idle`. Expect: no `━`, no `╽`/`┃`, no `▍`, and `theta-out` under a plain `│`.
- **Second view.** The recipe's `check.sh` also reads the journal: the `tool_end` row carries `theta-out`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/tool-motion`. Expect: `ok: comet while running, the output opened and its gutter filled, the reply dried behind a caret, idle rule plain, tool output in the journal`.

## Gotchas

- Opening lasts 500 ms and is eased out, so it is mostly open by 200 ms; the recipe snaps four times in a row right after `wait "✓ bash"`.
- The rule row can start with the comet's `━`, so a check that finds the rule must match `─` or `━`.
- A bash tail of more than four lines ends in a `(N lines)` count, which the four-line preview cuts to `… 1 more lines`.
- Match the box-drawing glyphs with `grep -e '╽' -e '┃'`, not a bracket class: in a byte locale `[╽┃]` matches bytes of the plain `│` too.
- Bash output reaches the TUI only when the tool ends (there is no live output event yet), so the unfurl plays then; a live tail while running would need one.
- The comet is not drawn while a permission panel is up: the panel takes the rule's place.
