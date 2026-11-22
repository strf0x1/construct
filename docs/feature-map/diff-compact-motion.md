---
id: diff-compact-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/compact.rs, crates/construct-tui/src/app_events.rs, crates/construct-tui/src/status.rs, crates/construct-tui/src/view.rs]
verified_at: 0fbd4ad
verified_deps: efb6111e28e1cca8
---
# Motion: diff highlighter and compaction

When an edit finishes, its diff trickles in like any tool output, then a white highlighter pass sweeps it left to right and the `+`/`-` lines settle to green and red. When construct compacts history, the transcript stays on screen (only what the model is sent shrinks), so nothing is dissolved away: a lavender wash over the transcript above the `◇ compacted history` notice clears from the top down into it, the notice resolves out of quadrant block glyphs, and when the next usage report shows the context share dropped by ten points or more, the `ctx` gauge drains down to it over ~0.9 s instead of jumping. `CONSTRUCT_MOTION=off` draws none of it (ADR 0065).

## Sub-features

- `motion-diff-highlight` a finished edit's diff gets one white highlighter pass left to right.
- `motion-compact-wave` the transcript above a compaction notice washes lavender and clears top-down into it.
- `motion-compact-notice` the notice resolves out of quadrant blocks.
- `motion-gauge-drain` a drop of ten points or more in the context share drains the gauge; a rise or a small drop shows at once.

## How to get to it (user POV)

- Ask for an edit and watch its diff. For compaction, run a long turn in a small window (`context_window = 16384`) until `◇ compacted history` appears.

## Done when

- Just after an edit, an added line is white mid-pass (`fg=#ebeeff`); later it is green.
- Just after `◇ compacted history` appears, its text is still partly quadrant blocks; later it reads whole.
- The gauge drain and the wave are pinned by `motion::compact` unit tests: a drop drains and lands on the new share, a rise does not, and a compaction targets the rows above its notice and the notice itself.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/diff-compact-motion`; `drive.env` sets `CONSTRUCT_MOTION=on`, `serve.args` gives the mock a 16k window, and `setup.sh` writes `router.rs`, three ~10 KB `big*.txt` files and a config with `context_window = 16384` (the setup of `crates/construct-dev/tests/compaction.rs`). The scripted model edits `router.rs` in turn one, then reads the big files twelve times in turn two.

- **Highlighter.** Drive: `type tidy the router`, `key enter`, `wait "✓ edit"`, `sleep 250`, `snap --style diff-1` … `diff-4`. Expect: the `g.push(Web)` line carries `fg=#ebeeff` in one snap.
- **Compaction.** Drive: `type read the big files`, `key enter`, `wait "compacted history"`, `snap --style compact-1` … `compact-4`. Expect: a quadrant glyph on a `compacted hist…` line in one snap; `idle` has every notice whole.
- **Second view.** The recipe's `check.sh` reads the journal: an edit `tool_end` (`edited`) and a `compacted` row. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/diff-compact-motion`. Expect: `ok: the diff got its highlighter pass, the compaction notice resolved, edit and compaction in the journal`.

## Gotchas

- The edit runs in its own turn: in one turn with the reads, it scrolls off screen before the snaps can see it.
- tachyonfx runs `fade_from_fg` and `coalesce` sweeps in reverse, so the wave uses `down_to_up` to clear top-first.
- A long turn in a 16k window compacts several times, so several notices (each with its wave) appear; that is construct's behavior, not the recipe's.
- The gauge drains only when a usage report lands after the drop, and only by ten points or more; the recipe does not time it.
