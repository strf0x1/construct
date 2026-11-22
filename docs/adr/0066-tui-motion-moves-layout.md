# 0066 — TUI motion may move layout

Status: accepted. 2026-11-22. Amends 0065.

## Context

ADR 0065 brought tachyonfx motion into the TUI with the rule that effects
recolor or blank cells and never move layout. All seven prototype scenes
landed that way, but side by side with the `motion-lab` prototype they read
flatter (#356). The issue first blamed the lack of borders and background
fills. Reading the prototype's source showed otherwise: like construct, it
boxed only the permission panel, and it set no background beyond the
panel's badge. What it did and construct did not:

- **Layout moved.** A tool's output opened row by row, so the transcript
  above was pushed up; the permission panel rose out of the bottom and the
  transcript slid back down once it was gone. In construct the output took
  its full height at `ToolEnd` and the "unfurl" was a reveal over rows
  already in place. A two-line output had almost nothing to reveal.
- **Streaming text dried.** New characters landed bright and cooled to ink
  over about 0.6 s behind a blinking caret. construct appended plain text.
- **Sweeps had rects the size of their content.** The logo's rect was the
  logo, so its glow took its whole time. construct gave every effect the
  full row, so the splash title had to drop its sweep.
- **The splash had a block-letter logo** where construct had `◆ construct`.

A live tail of bash output while it runs was the one remaining difference.
It needs a streaming output event from core and is left for later.

## Decision

- Motion may move layout, but only as a row count eased over at most about
  a second, read while the frame is laid out: `motion/grow.rs`.
  - A finished tool's output opens from its header down over 900 ms
    (ease-out), and `App::transcript` cuts the block to what has opened
    (`Motion::clip`). The bottom anchor and scroll treat it as any block
    that grew, so the transcript above is pushed up. A scrolled-back view
    stays still, as it does for any growth.
  - The permission panel rises over 320 ms. It is drawn whole at its final
    rect, then cut to its top rows at the foot (`motion::rise`), as if it
    came up from behind the status line. Its border draw and fade-in play
    at the same time, inside the 450 ms it ignores keys.
  - Once the panel is answered, its room is held while its ghost dissolves
    (420 ms), then closes over 300 ms, so the transcript slides down into
    place rather than jumping under the ghost.
- The reply being written dries from white to its own color over 600 ms
  (`motion/ink.rs`). Default-colored text dries toward a near-white ink and
  is then left alone. Bursts are spread over the time since the last one,
  at most 140 ms, so the tail reads as a stream. A caret (`▍`) blinks after
  the last character while the phase is writing. Cells are matched to
  characters by counting non-blank cells back from the end.
- `App::rows` holds a `motion::Place` per block: first row, rows, and the
  width of its widest line. Effects get rects that width plus two columns
  (for a shake's shift and the caret), so sweeps run over the text. The
  splash title gets back the prototype's radial resolve and a glow sweep.
- The splash title is the two-row block-letter logo when the terminal is
  wide enough (39 columns) and `◆ construct` otherwise. Its key hints show
  keys in accent. This is look, not motion: it shows with motion off too.
- Everything else in 0065 stands. Effects still set no background beyond
  the panel's own cells. `CONSTRUCT_MOTION=off` still draws the pre-motion
  screen: every row count is the full one, and nothing is tracked.

## Consequences

- Growth is a row count, never a new layout pass: a future effect that
  changes size goes in `grow.rs` and is read where the frame is laid out.
- While output opens or the panel rises, the loop draws at ~30 fps
  (`Motion::running` includes `moving` and `drying`), as for any effect.
- Drying assumes a dark theme, as the white flashes in 0065 already did. On
  a light theme the newest characters are briefly pale. On a dark theme
  whose default foreground is far from the ink color, default-colored text
  steps from ink to the terminal's own color once it has dried.
- Opening pushes the transcript up while the model may still be streaming
  below it. That is the prototype's look, and it lasts under a second. The
  prototype opened in 500 ms; driven live that read as too fast, so the
  opening, the trickle (1100 ms) and the gutter fill (800 ms) were slowed.
- `view.rs` lost `input_lines` to `input_line.rs`, and `motion/mod.rs` lost
  the rule and comet to `motion/rule.rs`, to stay under their ratchets.
- The golden first frame changed (the logo, `/help commands`).
- Revisit if growth fights scrolling in practice (a long expanded output
  opening while the user reads above it), or once core streams bash output.
  A live tail would replace the open on `ToolEnd` for commands.

## Evidence

- Prototype: `motion-lab` (seven scenes), compared scene by scene with the
  recipes below.
- Recipe `tool-motion`: PASS three runs in a row. The output caught opening
  (`unfurl-1`, 1 of 5 rows), the gutter filling, the reply's tail bright
  behind `▍`, idle with no caret, and `tool_end` with `theta-out` in the
  journal.
- Recipe `panel-motion`: PASS three runs in a row. The panel caught rising
  (12 or 13 of 14 rows in `arrive-1`), plus the existing arrival, selection,
  ghost and shake checks.
- Recipe `splash-motion`: PASS three runs in a row. The logo caught in
  shaded blocks spreading from its middle, settled whole, first frame under
  100 ms in `trace.log`.
- Recipe `diff-compact-motion`: PASS.
- Unit tests: `motion::grow` (opening, rise, room, off), `motion::ink`
  (drying, caret, burst spread, off).
