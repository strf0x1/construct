---
id: permission-prompt
status: verified
surfaces: [tui]
depends: [crates/construct/src/permission, crates/construct/src/tui/prompt.rs, crates/construct/src/tui/ask.rs, crates/construct-dev/tests/permission.rs]
verified_at: 24a6aad
---
# Permission prompt

When the model's tool call needs a path outside the sandbox, the TUI stops and shows a bordered PERMISSION panel naming who asks (the main agent), the tool and the paths, and four choices colored by consequence: allow once and allow for this session in green, always allow (saved to `config.toml`) in amber, deny in red. The selected choice is a filled bar that moves with the arrow keys; letter keys pick directly. When the path's folder could be allowed instead, a `scope` row offers `this path` or `its whole folder` (the chosen one cyan and filled), `tab` switches it, and the hint line adds `tab scope`; a grant for the folder covers its other files without asking again. The answer is shown as a notice and the tool call goes ahead or fails accordingly.

## Sub-features

- `permission-prompt-panel` the panel, badge, paths and colored choices appear for a blocked file tool.
- `permission-prompt-select` up/down move the filled selection bar; enter confirms it.
- `permission-prompt-keys` `y` `s` `a` `n` answer directly.
- `permission-prompt-scope` tab switches between this path and its whole folder.
- `permission-prompt-always` always allow writes the path to `[sandbox] allow_read`/`allow_write` in `config.toml`.
- `permission-prompt-shell` a blocked shell command asks, and allowing re-runs the command.
- `permission-prompt-grace` keys pressed in the first 450 ms after the panel appears are ignored.

## How to get to it (user POV)

- Ask construct to do something that reads or writes outside the working directory, e.g. "read /some/other/dir/a.txt"; the panel appears on its own.
- Inside the panel: `up`/`down` (or `k`/`j`) and `enter`; `y`, `s`, `a`, `n`; `tab` for scope; `esc` interrupts the turn.

## Done when

- The panel shows `PERMISSION`, `main agent`, the blocked path, and the four choices in their consequence colors, with the first choice filled.
- Moving the selection moves the fill; confirming "Allow for this session" shows `allowed for this session` and the model's next request carries the file's contents as the tool result.
- The journal has one `access` row with the chosen decision.
- "Always allow" leaves the path in `config.toml`; "Deny" gives the model an error result.
- With the scope switched to `its whole folder`, a later read of a sibling file goes ahead without a second panel, and the one `access` row names the folder.

## Driving it

Preconditions:

- Baseline state; `setup.sh` writes `{outside}/a.txt` containing `alpha`. Model: read `{outside}/a.txt`, then say `all done`. Recipe: `.agents/skills/verify-construct/recipes/permission-prompt`.

- **Trigger.** Ask for the read. Drive: `type read it`, `key enter`, `wait "PERMISSION" 8000`, `sleep 600`. Expect: the panel with `main agent` and the path.
- **Look.** Check the colors. Drive: `snap --style prompt`, `expect-style "Allow once" fg=#9ece6a bg=#2c3048 bold`, `expect-style "Always allow" fg=#e0af68`, `expect-style "Deny" fg=#f7768e`, `image {evidence}/prompt.png`. Expect: all checks pass; the PNG shows the panel.
- **Select.** Move down. Drive: `key down`, `sleep 100`, `expect-style "Allow for this session" bg=#2c3048 bold`, `expect-style "Allow once" bg=default !bold`. Expect: the fill moved.
- **Confirm.** Drive: `key enter`, `wait "allowed for this session"`, `wait "all done"`. Expect: the panel is gone and the turn finishes.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/permission-prompt` (runs it after the drive). Expect: `ok: tool result carried the file; access row decision=session` — the last `requests/req-*.json` holds `alpha` in its tool message, and `journal.txt` has one `access` row with `"decision":"session"`.

## Gotchas

- Keys sent within 450 ms of the panel appearing are dropped on purpose; always `sleep 600` first.
- The border breathes between two ambers, so `settle` never returns while the panel is open.
- `expect-style` checks the first occurrence on screen; the path or your typed message can contain the same words.
- The recipe drives `panel`, `select` and `grace`. The `keys`, `scope`, `always` and `shell` sub-features were proven at `verified_at` by `cargo test -p construct-dev --test it permission::` (the same binary in a pty, asserting screen, results, journal and `config.toml`); to see them, copy the recipe and change the keys.
