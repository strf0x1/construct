---
id: splash-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/shaders.rs, crates/construct-tui/src/motion/splash.rs, crates/construct-tui/src/splash.rs, crates/construct-tui/src/view.rs]
verified_at: e7b3fff
verified_deps: 4b741c84d0edcc30
---
# Motion: splash intro

At startup the splash plays a short intro over the first frame. The title, a three-row heavy geometric box-drawing `CONSTRUCT` wordmark in a blue-to-lavender gradient (`◆ construct` on a terminal under 39 columns) centered optically in the viewport, is traced left to right by a luminous white-to-cyan beam (no shaded block noise), followed by a soft diagonal specular sheen. Underneath, a tagline (`◆ lean · fast · learns as it goes`), a thin divider rule that draws itself out with a bright `━` tip, the aligned model/endpoint/router/session rows, and the key hints cascade in cleanly. It takes about 1.6 s and replaces the title's shimmer, which still plays with `CONSTRUCT_MOTION=off` (ADR 0065, 0066). The first frame waits on nothing: the intro only changes cells once a frame is drawn. The first real block (a turn, or a session restored by `--resume`) drops whatever is left of it.

## Sub-features

- `motion-splash-title` a luminous beam traces the heavy geometric wordmark left to right, then a soft diagonal sheen glides across it.
- `motion-splash-follow` the tagline, divider rule, session rows and hints cascade in cleanly after it, staggered.
- `motion-splash-yield` a real block on screen drops the intro.

## How to get to it (user POV)

- Start `construct` with no arguments in a fresh project.

## Done when

- In the first ~200 ms the wordmark is caught mid-trace with a luminous white leading edge (`fg=#ebeeff`), with no `░▒▓` shaded blocks on any frame, and the hints are not yet in.
- Settled, the splash shows the centered heavy geometric wordmark whole, the divider rule, the hints, and the configured router.
- `trace.log` puts the first frame well under 100 ms.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/splash-motion`; its `drive.env` sets `CONSTRUCT_MOTION=on` and `CONSTRUCT_TRACE=1`, and its `setup.sh` writes a config with `[router] mode = "local"`. The scripted model is never asked.

- **Intro.** Drive: `wait "openrouter"`, then `snap --style s-1` … `s-5`. Expect: `fg=#ebeeff` on wordmark strokes in one snap; no `░▒▓` in any snap; no `enter send` in `s-1`.
- **Settled.** Drive: `sleep 1800`, `snap --style settled`. Expect: the logo's first row `┏━━ ┏━┓ ┏┓┃ ┏━━ ━┳━ ┏━┓ ┃ ┃ ┏━━ ━┳━`, the divider rule `───────────`, `enter send`, `router   local`.
- **Second view.** The recipe's `check.sh` reads `$CONSTRUCT_HOME/trace.log`: the `first frame` milestone is under 100 ms. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/splash-motion`. Expect: `ok: luminous beam traced the heavy logo without blocks, hints cascaded in after, first frame undelayed, router from config shown`.

## Gotchas

- Effect rects carry both `Place::left` and `Place::width`, so sweeps over the horizontally centered splash spend their full duration on the actual strokes rather than the empty margins.
- `beam_reveal` and `rule_draw` blank `dx == 0` too while `alpha == 0.0`, so `prolong_start` delays never leak the first character of a staggered part.
- The intro is dropped, not replayed, if a block appears before it ends; `--resume` restores blocks right after the first frame, so a resumed session shows little or none of it.
