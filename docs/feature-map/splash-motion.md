---
id: splash-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/splash.rs, crates/construct-tui/src/splash.rs, crates/construct-tui/src/view.rs]
verified_at: caeb8f5
verified_deps: 1ab341d451e8bde4
---
# Motion: splash intro

At startup the splash plays a short intro over the first frame. The title, a two-row block-letter `CONSTRUCT` in a blue-to-lavender gradient (`◆ construct` on a terminal under 39 columns), resolves out of shaded blocks (`░▒▓`) spreading from its middle outward, and then a white glow sweeps it once, left to right. The tagline, the model/endpoint/router rows and the key hints then fade in after it, each a beat later. It takes about 2 s and replaces the title's shimmer, which still plays with `CONSTRUCT_MOTION=off` (ADR 0065, 0066). The first frame waits on nothing: the intro only changes cells once a frame is drawn. The first real block (a turn, or a session restored by `--resume`) drops whatever is left of it.

## Sub-features

- `motion-splash-title` the title resolves out of shaded blocks from its middle, then a white glow sweeps it.
- `motion-splash-follow` the tagline, session rows and hints fade in after it, staggered.
- `motion-splash-yield` a real block on screen drops the intro.

## How to get to it (user POV)

- Start `construct` with no arguments in a fresh project.

## Done when

- In the first ~200 ms the title row shows shaded block glyphs, and the hints are not yet in.
- Settled, the splash shows the block-letter title whole, the hints, and the configured router, with no shaded glyphs left.
- `trace.log` puts the first frame well under 100 ms.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/splash-motion`; its `drive.env` sets `CONSTRUCT_MOTION=on` and `CONSTRUCT_TRACE=1`, and its `setup.sh` writes a config with `[router] mode = "local"`. The scripted model is never asked.

- **Intro.** Drive: `wait "openrouter"`, then `snap --style s-1` … `s-5`. Expect: `░`, `▒` or `▓` in one snap; no `enter send` in `s-1`.
- **Settled.** Drive: `sleep 2000`, `snap --style settled`. Expect: the logo's first row `▄▀▀ ▄▀▄ █▄ █ ▄▀▀ ▀█▀ █▀▄ █ █ ▄▀▀ ▀█▀`, `enter send`, `router   local`, no `░▒▓`.
- **Second view.** The recipe's `check.sh` reads `$CONSTRUCT_HOME/trace.log`: the `first frame` milestone is under 100 ms. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/splash-motion`. Expect: `ok: the title resolved out of blocks, the hints came in after it, first frame undelayed, router from config shown`.

## Gotchas

- The logo is drawn with `▄▀█`, so a check for the intro's shaded blocks must look for `░▒▓` only, never `█`.
- Effect rects end at the content's widest line (`motion::Place::width`), which is what lets the title's sweep take its full time; an older version swept the whole row and was past the title in ~100 ms.
- `evolve_into` starts below the lightest shade, so the title row is blank on the very first frame, then shaded, then text.
- The intro is dropped, not replayed, if a block appears before it ends; `--resume` restores blocks right after the first frame, so a resumed session shows little or none of it.
