---
id: panel-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/panel.rs, crates/construct-tui/src/motion/shaders.rs, crates/construct-tui/src/view.rs, crates/construct-tui/src/app.rs, crates/construct-tui/src/prompt.rs, crates/construct-tui/src/theme.rs]
verified_at: 97f158c
verified_deps: 3cab4bd3dc0ab689
---
# Motion: permission panel and outcomes

A permission panel arrives by drawing its own border: two arms grow from the bottom middle, climb both sides and meet at the top, with bright heads, while the contents fade in left to right. It all finishes inside the 450 ms the panel ignores keys. Moving the selection sweeps the newly selected row white once. Answering leaves a ghost of the panel over the screen that replaced it: its glyphs wash green when the call was let through (amber when denied, dim when the question went stale) and drop out cell by cell, showing the input beneath. A failed tool's header flashes pale red and shakes a couple of columns side to side before it settles, and its error lines come out of a hue glitch as they trickle in. `CONSTRUCT_MOTION=off` draws none of it (ADR 0065).

## Sub-features

- `motion-panel-arrive` the panel's border draws itself from the bottom middle and its contents coalesce left to right.
- `motion-panel-select` a moved selection sweeps the new row white.
- `motion-panel-dissolve` an answered panel dissolves as a ghost, washed green (allowed) or amber (denied).
- `motion-fail-shake` a failed tool's header shakes and settles at its indent.

## How to get to it (user POV)

- Ask for anything that makes a tool reach past the sandbox (a read outside the workspace), move the selection, answer; then watch a tool fail.

## Done when

- Just after the panel appears, its top edge has a gap of spaces where the two arms have not met.
- Just after `down`, the `Allow for this session` row carries the white sweep (`fg=#ebeeff`).
- Just after `enter`, the panel's edge is still on screen washed green (`fg=#9ece6a`); later it is gone.
- Just after a tool fails, its `✗ bash` header is off its two-space indent; idle, it is back at it.

## Driving it

Preconditions:

- Baseline state, motion on. Recipe: `.agents/skills/verify-construct/recipes/panel-motion`; its `drive.env` sets `CONSTRUCT_MOTION=on`, its `setup.sh` writes `outside/a.txt` and a local-router config. The scripted model reads `{outside}/a.txt` (which asks), then runs a shell command that sleeps a second and exits 3, then replies.

- **Arrival.** Drive: `type read it`, `key enter`, `wait "PERMISSION"`, `snap --style arrive-1` … `arrive-3`. Expect: a gap (`─ +─`) in the top edge in one snap; `snap --style settled` after 600 ms has none.
- **Selection.** Drive: `key down`, `snap --style sel-1`, `sel-2`. Expect: `fg=#ebeeff` on the `Allow for this session` row.
- **Dissolve.** Drive: `key up`, `sleep 400`, `key enter`, `snap --style ghost-1` … `ghost-4`. Expect: a green (`fg=#9ece6a`) `╰`, `╯` or `│` of the panel's edge in one snap.
- **Shake.** Drive: `wait "✗ bash"`, `snap --style fail-1` … `fail-4`. Expect: the `✗ bash` line not at `^  ✗ bash` in one snap; `idle` has it back.
- **Second view.** The recipe's `check.sh` also reads the journal (the read's `tool_end` returned `alpha`; a `tool_end` with `"ok":false`) and the transcript (`⚠ allowed once`). Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/panel-motion`. Expect: `ok: border drew itself, selection swept, the answered panel dissolved green, the failure shook; read allowed and shell failed in the journal`.

## Gotchas

- The arms climb the sides first and meet at the top middle, so the top-right corner is drawn early; look for the gap in the middle of the top edge, not a missing corner.
- The ghost covers the transcript rows where the answer's notice lands, so a ghost snap does not show `⚠ allowed once`; the check reads that from the whole transcript.
- The failing command sleeps a second first: without it, the tool fails while the ghost snaps (and their PNG) are still being taken, and the 380 ms shake is over before `wait "✗ bash"` looks.
- A queued second question replaces the panel in place: the new one draws its border again and the old one leaves no ghost.
