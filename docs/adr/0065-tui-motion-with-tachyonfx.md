# 0065 — TUI motion with tachyonfx

Status: accepted. 2026-11-21.

## Context

The TUI already moves a little, all of it hand-rolled in `theme.rs`: a spinner,
a shimmer across the activity label, a pulse on the rule while a turn runs.
We wanted motion that also tells the user something: which phase a turn is in
at a glance, and where a finished tool's output landed. A prototype of seven
effects (tool output unfurling, a comet on the rule, a permission panel that
draws itself, outcome flashes, a diff highlighter, visible compaction, a
splash) was built on tachyonfx 0.25, a ratatui post-processing library, to
see what it can and cannot do here.

What it showed:

- construct draws foreground-only on the terminal's own background, so any
  effect that fades to or from a background color (`sweep_in`, `slide_in`,
  `fade_from` with a bg) is out: we do not know the color. Effects that only
  recolor glyphs or swap symbols (`fade_from_fg`, `coalesce`, `dissolve`,
  `evolve`, custom `effect_fn_buf` shaders) work on any theme.
- tachyonfx transforms cells inside a rect after the frame is drawn; it never
  moves layout. Anything that changes size (a box opening, a gauge draining)
  is ours to ease.
- It builds on `ratatui-core` 0.1, the same core ratatui 0.30 uses, so the
  buffer types are shared and nothing is converted.

Rule 8 says a dependency must remove more code than it adds. tachyonfx does not
on the first two effects alone; it buys timers, interpolation, patterns
(sweeps, radial), and the coalesce/dissolve/evolve family that the later
effects in the prototype need, and that we would otherwise write and test
ourselves.

## Decision

- `construct-tui` depends on `tachyonfx` with `default-features = false,
  features = ["std"]` (no DSL parser).
- All motion lives in `crates/construct-tui/src/motion.rs` (`Motion`). Effects
  are applied over a finished frame and target a block by index, looked up in
  `App::rows` each frame, so they follow scrolling. `view.rs` only calls in.
- Effects recolor or blank cells; they never set a background and never move
  layout.
- `CONSTRUCT_MOTION=off` (`0`, `false`) draws exactly the pre-motion screen:
  `Motion` adds nothing and the rule keeps its old pulse. The e2e `Env` and the
  verify-construct `drive.sh` set it, so goldens and recipes do not depend on
  when a frame was taken; a recipe opts back in with a `drive.env` line.
- While an effect runs the loop draws at ~30 fps (a 33 ms arm beside the 50 ms
  animation tick); with nothing running, nothing extra is drawn.
- First two effects: the comet on the rule (its color is the phase, shared
  with the activity line through `motion::hue`) and the tool output unfurl on
  `ToolEnd` (outcome flash on the header, gutter fill, top-down coalesce).

## Consequences

- New motion is an effect in `motion.rs` plus a trigger, not a change to the
  renderers. The rest of the prototype (panel, outcomes, diff, compaction,
  splash) can land the same way.
- A live tail of bash output while it runs needs a streaming output event from
  core; today output arrives only at `ToolEnd`, so the unfurl plays then.
- One duplicate crate: `compact_str` 0.10 (tachyonfx) beside 0.9
  (ratatui-core); `cargo deny` warns, as for the existing duplicates.
- Revisit if tachyonfx stops tracking ratatui's core, or if the effects we keep
  shrink to a few we could hand-roll in fewer lines than the dependency costs.

## Evidence

- Prototype: `motion-lab` (seven scenes, frames captured with
  `construct-dev drive`).
- Recipe `tool-motion` (`.agents/skills/verify-construct/recipes/tool-motion`):
  PASS, run `motion2`; comet head on the rule while bash ran, gutter caught
  filling in `unfurl-1`, idle rule plain, `tool_end` with the output in the
  journal.
- `cargo deny check`: advisories, bans, licenses, sources ok.
