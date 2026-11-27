---
id: peer-motion
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/motion/peers, crates/construct-tui/src/motion/mod.rs, crates/construct-tui/src/motion/shaders.rs, crates/construct-tui/src/peers, crates/construct-tui/src/view.rs, crates/construct-tui/src/theme.rs]
verified_at: e7b3fff
verified_deps: 2fe81663aabd7ef7
---
# Motion: peers tile, popup and attach view

The peer surfaces carry the same motion vocabulary as the rest of the TUI (ADR 0065, PR #397). New rows in the upper-right peer tile beam in left to right over the pill background, state changes react (green flash and sheen on joining or finishing a turn, cyan sweep on starting work, red flash and shake on revocation, lavender sweep on a `--model` switch, orange ping on a new question), a `working` peer spins an orbital indicator (`◐ ◓ ◑ ◒`) with a soft cyan shimmer, a `pending` peer pulses its diamond with a glint over its SAS code, and `?N` pulses amber. The JOIN and PEER popups trace their cyan borders from the bottom middle while beaming their contents in (no shaded blocks; JOIN adds a specular sheen once revealed), sweep moved selections white across wrapped rows, and dissolve as confirm-pop ghosts washed green (approve/allow), amber (always), red (deny), or dim (`esc`). `/peer <name>` tunes in with a cyan border draw and beam reveal, runs a cyan border comet while the peer works, applies top-edge scroll fade to scrolled output, highlights fresh incoming lines, and dissolves cyan on `esc` without disturbing the peer tile. `CONSTRUCT_MOTION=off` draws none of it.

## Sub-features

- `motion-peer-tile` tile rows beam in, react to state/model/question changes (including shake on revoke), and run live orbital/pulse indicators for working, pending, and questioning peers.
- `motion-peer-popup` JOIN and PEER popups draw their cyan border, beam in cleanly, sweep moved selections, and dissolve with a confirm-pop ghost tinted by outcome.
- `motion-peer-attach` `/peer <name>` tunes in with a cyan border draw and beam reveal, runs a working border comet, scroll-fades top rows, and dissolves cyan on `esc` beneath the tile.

## How to get to it (user POV)

- Run `construct hub` with a peer joining, approving it, sending a prompt that triggers a peer permission question, attaching with `/peer <name>`, and switching its model with `/peer <name> --model <selector>`, with motion on (default).

## Done when

- The JOIN popup arrives without shaded blocks, moving selection to `Deny` sweeps it white (`fg=#ebeeff`), and approving leaves a green-washed (`fg=#9ece6a`) dissolving ghost.
- While the peer works, the tile displays the orbital working glyph (`◐`/`◓`/`◑`/`◒`) instead of static `◉`.
- In `/peer <name>` with more output than fits the viewport, the top visible row is scroll-faded darker than lower rows; pressing `esc` dissolves the attach view without clobbering the tile.
- All underlying hub/peer actions (`hub/peers.json`, peer journal `tool_end` and `access`) succeed.

## Driving it

Preconditions:

- `drive.env` sets `CONSTRUCT_MOTION=on`; `setup.sh` starts a real hub with pairing open and a peer `old-pc` whose scripted model reads `{outside}/secret.txt` then returns 26 lines of output. Recipe: `.agents/skills/verify-construct/recipes/peer-motion`.

- **Join motion.** Drive: `wait "JOIN · old-pc"`, `snap --style arrive-1` … `arrive-3`, `sleep 650`, `key down`, `snap --style sel-1`, `sel-2`, `key up`, `key enter`, `snap --style ghost-1` … `ghost-4`. Expect: no shaded blocks in arrival, `Deny` swept white (`fg=#ebeeff`), green ghost (`fg=#9ece6a`) after approve.
- **Working tile & ask.** Drive: `type /peer old-pc read the secret`, `key enter`, `wait "about its own machine"`, `sleep 600`, `snap --style ask-settled`, `type y`. Expect: orbital glyph (`◐`/`◓`/`◑`/`◒`) on `old-pc working` in `ask-settled`.
- **Attach motion & model.** Drive: `type /peer old-pc`, `key enter`, `wait "peer finished"`, `snap --style attach-settled`, `key esc`, `snap --style attach-close-1`, `type /peer old-pc --model small`, `key enter`. Expect: scroll-faded top row in `attach-settled`, crisp peer tile during `attach-close-1`, and `small (hub)` in the tile.
- **Second view.** The recipe's `check.sh` verifies both the styled snaps and the real hub registry (`hub/peers.json`) and peer journal (`tool_end`, `access`). Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/peer-motion`. Expect: `ok: peer beam arrival, selection sweep, green dissolve ghost, orbital working tile, attach scroll fade, and real hub/peer state verified`.

## Gotchas

- `peers/tile.rs` draws `◉` for working peers; the orbital glyph (`◐`/`◓`/`◑`/`◒`) is applied only when `CONSTRUCT_MOTION=on`, so `peer-tile` (motion off) keeps static `◉`.
- The peer tile overlaps the top-right corner of the attach box; `motion::peers` saves and restores the tile rect around attach effects and close ghosts so the tile never flickers.
