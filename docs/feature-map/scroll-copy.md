---
id: scroll-copy
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/clip.rs, crates/construct/src/tui/view.rs, crates/construct/src/tui/input.rs]
verified_at: 066a40d
---
# Scroll and copy

The transcript scrolls with the mouse wheel, `up`/`down` and `pgup`/`pgdn` without construct capturing the mouse, so the terminal's own text selection keeps working. `esc` (when no turn is running) jumps back to the bottom. `/copy` puts the last reply on the system clipboard through the terminal (OSC 52, which also works over SSH) and says how many characters it copied; `/copy error` copies the last failed tool's output and `/copy status` the `/status` block.

## Sub-features

- `scroll-wheel` the wheel scrolls the transcript: the terminal sends it as up/down keys (alternate scroll mode), and mouse capture stays off.
- `scroll-keys` `up`/`down` scroll 3 lines, `pgup`/`pgdn` 10.
- `scroll-bottom` `esc` while idle returns to the newest line.
- `copy-reply` `/copy` sends the last assistant reply as OSC 52 and shows `copied last reply (N chars)`.
- `copy-error` `/copy error` copies the last failed tool's output; `no last tool error to copy` when there is none.
- `copy-status` `/copy status` copies the `/status` block.

## How to get to it (user POV)

- Scroll with the mouse wheel over the transcript, or `up`/`down`/`pgup`/`pgdn`; `esc` to return.
- Type `/copy`, `/copy error` or `/copy status` and press enter.

## Done when

- After a long reply, scrolling up shows earlier lines and hides the newest; `esc` brings the newest back.
- `/copy` shows `copied last reply` and the terminal received an OSC 52 sequence; mouse capture was never turned on, and alternate scroll was.

## Driving it

Preconditions:

- Baseline state. Model: one text reply of `line-01` … `line-80`, one per paragraph. Recipe: `.agents/skills/verify-construct/recipes/scroll-copy`.

- **Turn.** Drive: `type go`, `key enter`, `wait "line-80" 5000`. Expect: the reply, scrolled to its end.
- **Scroll.** Drive: `key up` ten times, `wait "line-5" 2000`, `gone "line-80" 2000`. Expect: earlier lines on screen, the last one gone.
- **Return.** Drive: `key esc`, `wait "line-80" 2000`. Expect: back at the bottom.
- **Copy.** Drive: `type /copy`, `key enter`, `wait "copied last reply" 2000`, `image {evidence}/scroll-copy.png`. Expect: the notice with the character count.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/scroll-copy`. Expect: `ok: alt-scroll on, capture off, OSC 52 sent` — `raw.log` holds `ESC[?1007h`, no `ESC[?1000h`, and an `ESC]52;c;` sequence.

## Gotchas

- The pty driver has no mouse; the wheel is proven by alternate scroll being on (the terminal turns the wheel into the same up/down keys the drive sends), not by a wheel event.
- OSC 52 has no reply: the terminal may ignore it, so the proof is the bytes in `raw.log`, not the clipboard.
- `up`/`down` always scroll (input history is `ctrl+p`/`ctrl+n`), except while the `/` palette is open, where they move its selection.
- `copy-error`, `copy-status` and `scroll-keys`' `pgup`/`pgdn` are not driven by the recipe; add steps to the recipe before changing them.
