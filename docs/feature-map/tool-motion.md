---
id: tool-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/grow.rs, crates/construct-tui/src/motion/ink.rs, crates/construct-tui/src/motion/rule.rs, crates/construct-tui/src/motion/shaders.rs, crates/construct-tui/src/tool_block.rs, crates/construct-tui/src/view.rs, crates/construct-tui/src/app_events.rs, crates/construct-tui/src/input.rs, crates/construct-tui/src/lib.rs, crates/construct-core/src/tools/shell.rs, crates/construct-core/src/tools/call.rs]
verified_at: e7b3fff
verified_deps: 4fafb5b07893fb21
---
# Motion: comet, live tail, unfurl, scroll fade and drying text

While a turn runs, a comet (a bright head with a fading tail) laps the rule above the input, in the color of what the turn is doing: lavender thinking, yellow running a tool, blue waiting or writing, cyan routing. It is the activity line's highlight color, so the two agree. When the turn ends the comet docks with one soft flash and the rule is plain again. While `bash` runs, stdout/stderr streams live (`Event::ToolOutput`, non-journaled) into a rolling 5-line tail under the spinning header where the newest line lands bright and fades darker as newer lines scroll it upward. When the command finishes, its header flashes its outcome and the live tail smoothly collapses into a compact 2-line preview with `… N more lines (ctrl+o)`. Tools that finish without streaming unfurl row by row from their header down while the `│` gutter fills with a bright head. Toggling `ctrl+o` eases tool details open and closed. As transcript text scrolls past the top edge of the viewport, the top few rows gently fade darker. The reply that follows dries as it streams: its newest characters land bright and cool to their own color behind a blinking `▍` caret. `CONSTRUCT_MOTION=off` turns all of it off and draws the plain screen (ADR 0065, 0066).

## Sub-features

- `motion-comet` a comet laps the rule while a turn runs, colored by the phase.
- `motion-dock` idle, the rule is plain faint `─` again after one flash.
- `motion-live-tail` a running `bash` streams a rolling 5-line tail whose newest line arrives bright and fades darker as lines scroll up, then collapses smoothly into a compact 2-line preview on finish.
- `motion-unfurl` a non-streaming finished tool's output opens row by row from its header down, its gutter fills with a bright `╽`/`┃` head and its text coalesces top-down.
- `motion-expand-ease` toggling `ctrl+o` eases tool details open and closed rather than snapping.
- `motion-scroll-fade` when the transcript scrolls past the top of the viewport, the top three rows softly fade darker.
- `motion-ink` a streaming reply's tail is bright and dries to ink, with a `▍` caret after it while the model writes.
- `motion-off` `CONSTRUCT_MOTION=off` (or `0`, `false`) disables motion; tests and recipes run with it off unless a recipe's `drive.env` turns it on.

## How to get to it (user POV)

- Send any prompt that runs a tool and watch the rule above the input, the live tail under `bash`, and the collapsed preview; press `ctrl+o` to ease details open or closed.

## Done when

- While the tool runs, the rule carries one white `━` head with a colored tail and the live tail shows recent lines under `bash` with the newest line brighter than `FAINT`.
- Just after `bash` finishes, the tail shrinks smoothly from 5 rows down to the 3-row compact preview (`eta-out`, `theta-out`, `… 6 more lines (ctrl+o)`).
- While the reply streams, its line ends in `▍` with the characters before it bright (red and green channels `d0` or more).
- Idle, the rule has no `━`, the output shows the settled compact preview with a plain `│`, and there is no `▍`.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/tool-motion`; its `drive.env` sets `CONSTRUCT_MOTION=on`, its `setup.sh` writes a local-router config, and the scripted model runs one shell command that prints eight words spaced 140 ms apart, then replies slowly (`serve.args`: `--chunk-ms 90`).

- **Comet & live tail.** Drive: `type list eight words`, `key enter`, `wait "zeta-out"`, `snap --style busy`. Expect: the rule row holds `⟦fg=#ebeeff⟧━`, 4+ live rows show under `bash`, and the newest line is brighter than `FAINT`.
- **Collapse.** Drive: `wait "✓ bash"`, then `snap --style settle-1` … `settle-3`, `sleep 500`, `snap --style settled`. Expect: `settled` has 3 rows (`eta-out`, `theta-out`, `… 6 more lines (ctrl+o)`), and an earlier `settle-*` snap has more rows mid-collapse.
- **Ink.** Drive: `wait "listed eight"`, `snap --style stream-1` … `stream-4` 120 ms apart. Expect: one snap's `▍` line has a bright `fg` on the characters before it.
- **Idle.** Drive: `wait "ask construct anything… (/help)"`, `sleep 1500`, `snap --style idle`. Expect: no `━`, no `╽`/`┃`, no `▍`, and `theta-out` under a plain `│`.
- **Second view.** The recipe's `check.sh` also reads the journal: the `tool_end` row carries `theta-out`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/tool-motion`. Expect: `ok: comet while running, live tail streamed and collapsed to compact preview, reply dried behind caret, idle rule plain, tool output in journal`.

## Gotchas

- `Event::ToolOutput` is non-durable (`Event::durable() == false`) and throttled to ~30 fps in `tools::call::OutputStream`, so runaway output never bloats the journal or starves the UI loop.
- The live tail holds for 120 ms after `ToolEnd` so the green `✓` flash registers before shrinking over 420 ms.
- `scroll_fade` applies only when `top > 0` and scales in smoothly over the first three scrolled-off lines; it never sets a background color.
- The comet is not drawn while a permission panel is up: the panel takes the rule's place.
