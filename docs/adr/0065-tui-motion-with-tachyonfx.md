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
- Then the permission panel and outcomes: the panel's border draws itself
  and its contents fade in within the 450 ms it ignores keys; a moved
  selection is found in the drawn frame by `prompt::SELECTED_BG`, so the
  prompt's layout code knows nothing of motion; an answered panel's last
  frame is kept and dissolves over what replaced it (`rule()` is drawn last
  for that), washed green when allowed and amber when denied. A failed tool's
  header shakes and its error lines settle out of a hue glitch.
- Then the diff highlighter and compaction: a finished edit's diff gets a
  white highlighter pass. Compaction keeps the transcript on screen (only
  what the model is sent shrinks), so motion must not dissolve rows away: a
  lavender wash above the notice clears top-down into it, the notice
  resolves out of block glyphs, and a drop of ten points or more in the
  context share drains the gauge over 0.9 s. With motion off the gauge shows
  the share as before.
- Last, the splash: the title resolves out of shaded blocks and glows, and
  the tagline, session rows and hints fade in after it. `App::splash` records
  its parts in `App::rows`; the intro is queued only by `Motion::from_env`
  and drawn only while the splash shows, so the first real block drops it.
  With motion on it replaces the title's shimmer.

## Consequences

- New motion is an effect in `motion/` plus a trigger, not a change to the
  renderers. All seven prototype scenes have landed this way.
- A sweep pattern spans the rect it is given. Where the content is a few
  cells of a terminal-wide row (the splash title), a sweep is over in a
  fraction of its time; such effects run without one.
- tachyonfx runs `fade_from_fg` and `coalesce` sweeps in reverse: a sweep
  that should clear top-first is written `down_to_up`.
- The ghost puts back the panel's own cells, backgrounds included (the badge,
  the selected bar), for under half a second; nothing else sets a
  background.
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
- Recipe `panel-motion`: PASS three runs in a row (`panel6`–`panel8`);
  border gap caught arriving, white sweep on the moved row, green ghost after
  enter, the failed header off its indent; the journal shows the read
  allowed and the shell failed.
- Recipe `diff-compact-motion`: PASS three runs in a row (`dc3`–`dc5`); the
  added diff line white mid-pass, a compaction notice caught resolving out of
  quadrant blocks, and the edit's `tool_end` and a `compacted` row in the
  journal.
- Recipe `splash-motion`: PASS three runs in a row (`splash4`–`splash6`); the
  title caught in shaded blocks, hints absent at the first snap, the settled
  splash whole with the configured router, and `trace.log`'s first frame at
  about 2 ms (one run at 10 ms). An earlier version with a sweep on the title
  failed two runs of three: its blocks were gone in ~100 ms.
- `cargo deny check`: advisories, bans, licenses, sources ok.
