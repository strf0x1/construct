---
id: slash-commands
status: verified
surfaces: [tui]
verified_at: ffa04de
---
# Slash commands

Typing a line that starts with `/` at the prompt runs a built-in command instead of sending a message: `/help` lists commands and keys, `/skills` lists the skill library, `/memory` shows memory, `/model <id>` switches the model for the next turn, `/learn` runs the learning pass, `/new` starts a fresh session, `/quit` exits. Each prints a notice in the transcript; an unknown command prints a yellow hint to try `/help`.

## Sub-features

- `slash-help` `/help` (or `/?`) draws the commands and keys as two rounded boxes: command names bold blue, arguments yellow, key names bold cyan.
- `slash-skills` `/skills` lists skills, or says there are none yet.
- `slash-model` `/model <id>` switches the model; the notice and the status line show it.
- `slash-memory` `/memory` prints memory or says it is empty.
- `slash-new` `/new` (or `/clear`) clears the transcript and counters and starts a new session.
- `slash-quit` `/quit`, `/exit` or `/q` exits.
- `slash-unknown` any other `/word` prints `unknown command /word — try /help` in yellow.

## How to get to it (user POV)

- Type the command at the `ask construct anything… (/help)` prompt and press enter.

## Done when

- `/help` shows every command; `/skills` shows the library state; `/model x` shows `model → x` in blue and `x` in the status line; an unknown command shows the yellow hint. None of these sends a model request.

## Driving it

Preconditions:

- Baseline state (no skills). Recipe: `.agents/skills/verify-construct/recipes/slash-commands`.

- **Help.** Drive: `type /help`, `key enter`, `wait "/new"`, `expect "╭─ commands"`, `expect-style "/skills" fg=#7aa2f7 bold`, `expect-style "ctrl+o" fg=#7dcfff bold`, `image {evidence}/help.png`. Expect: a `commands` box and a `keys` box, rows aligned in columns.
- **Skills.** Drive: `type /skills`, `key enter`, `wait "no skills yet"`. Expect: the empty-library notice.
- **Model.** Drive: `type /model verify-model-x`, `key enter`, `wait "model → verify-model-x"`, `expect-style "model → verify-model-x" fg=#7aa2f7`. Expect: the notice in blue and the status line showing `verify-model-x`.
- **Unknown.** Drive: `type /nope`, `key enter`, `expect-style "unknown command /nope" fg=#e0af68`, `image {evidence}/slash.png`. Expect: the yellow hint.
- **Memory, new, quit.** Drive: `type /memory`, `key enter`, `wait "memory is empty"`, `type /new`, `key enter`, `gone "unknown command /nope"`, `type /quit`, `key enter`, `gone "ask construct" 3000`. Expect: the memory notice, a cleared transcript, then construct exits.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/slash-commands`. Expect: `ok: no model requests` — `requests.jsonl` is empty.

## Gotchas

- `/skills <name>` and `/<skill-name>` do nothing yet (issue #63).
- `/model` with no argument does nothing (issue #69).
