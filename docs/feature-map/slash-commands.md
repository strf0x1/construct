---
id: slash-commands
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/help.rs, crates/construct/src/tui/palette.rs, crates/construct/src/tui/input.rs, crates/construct/src/tui/listing.rs, crates/construct/src/tui/boxed.rs, crates/construct/src/skills.rs]
verified_at: 09cf3c3
---
# Slash commands

Typing a line that starts with `/` at the prompt runs a built-in command instead of sending a message: `/help` lists commands and keys, `/skills` lists the skill library, `/memory` shows memory, `/model` opens the model picker (see [Model picker](./model-picker.md)) and `/model <id>` switches the model for the next turn, `/models [query]` searches the model list, `/learn` runs the learning pass, `/new` starts a fresh session, `/quit` exits. Each prints a notice in the transcript; the listing commands (`/models`, `/memory`, `/skills`, `/status`) draw rounded boxes like `/help`. `/<skill-name> <request>` runs a skill from the library: the model gets the skill's SKILL.md and the request in one user message. Typing `/` opens a palette under the input that lists commands and skills and narrows as you type. A word that is neither a command nor a skill prints a yellow hint to try `/help`.

## Sub-features

- `slash-help` `/help` (or `/?`) draws rounded boxes for commands, skills (only when there are any) and keys: command names bold blue, skill names bold green, arguments yellow, key names bold cyan.
- `slash-skills` `/skills` lists skills (project kit and global) in a green `skills` box, or says there are none yet. `/skills <name> <request>` runs one.
- `slash-palette` typing `/word` lists matching commands (blue) and skills (green, `skill ·` before the description), best match first: name prefix, then substring, then letters in order, then description. ↑↓ move the selection, tab completes it, esc closes the list. Enter runs a command that takes no required argument (`/model` opens the picker); for a skill it completes the name plus a space and waits for the rest. Past 8 rows a faint `… N more` row says how many are hidden.
- `slash-skill-run` `/<skill> <request>` runs a turn with the skill loaded and journals an `invoke` row (`skill`, `scope`). The library is read from disk each time, so a skill written mid-session can be run at once. A built-in command wins over a skill with the same name; `/skills <name>` still reaches that skill.
- `slash-model` `/model <id>` switches the model; the notice (`model → <id> · <window> ctx`) and the status line show it. With no argument it opens the picker. After `/model ` and a word the palette lists matching catalog models (the user's `[models.*]`, then the endpoint's list; the picker's matching), each with its window and label, narrowed per keystroke; enter or tab completes `/model <id>` and closes the list, a second enter switches. No match shows nothing and any id still works. Bare `/model ` stays the picker's. Recipe: `.agents/skills/verify-construct/recipes/model-palette`.
- `slash-models` `/models [query]` searches the cached model list (tool-capable models whose id or name contains every word) and draws a `models` box: id, window and USD per million tokens in and out, one row each (20 at most, then a dim `… and N more` line). On OpenRouter the list is fetched at startup and laid under the model catalog, so `/model <id>` and the picker use its windows unless the user's own `[models.*]` entry or a top-level `context_window` says otherwise (ADR 0020). `construct models [query] [--refresh]` prints the same from the shell. Recipe: `.agents/skills/verify-construct/recipes/models`.
- `slash-memory` `/memory` draws one box per non-empty store (titled with its use of the cap, `N/M chars`, one `•` row per entry) or says memory is empty.
- `slash-new` `/new` (or `/clear`) clears the transcript and counters and starts a new session.
- `slash-quit` `/quit`, `/exit` or `/q` exits.
- `slash-unknown` any other `/word` (not a command or a skill) prints `unknown command /word — try /help` in yellow.

## How to get to it (user POV)

- Type the command at the `ask construct anything… (/help)` prompt and press enter, or type `/` and pick from the palette.

## Done when

- `/help` shows every command; `/skills` shows the library state; `/model x` shows `model → x · 131k ctx` in blue and `x` in the status line; an unknown command shows the yellow hint. None of these sends a model request. The palette shows `▸ /create-verification` first for `/cre` when that skill exists, and `/create-verification do X` sends one request whose last user message holds the SKILL.md and `do X`.

## Driving it

Preconditions:

- Baseline state (no skills). Recipe: `.agents/skills/verify-construct/recipes/slash-commands`.

- **Help.** Drive: `type /help`, `key enter`, `wait "/new"`, `expect "╭─ commands"`, `expect-style "/skills" fg=#7aa2f7 bold`, `expect-style "ctrl+o" fg=#7dcfff bold`, `image {evidence}/help.png`. Expect: a `commands` box and a `keys` box, rows aligned in columns.
- **Skills.** Drive: `type /skills`, `key enter`, `wait "no skills yet"`. Expect: the empty-library notice.
- **Model ids.** Recipe `model-palette` (OpenRouter dialect on the mock, session starts on `mock`). Drive: `type /model mock/`, `wait "▸ mock/small"`, `expect "32.8k"`, `image {evidence}/model-palette.png`, `key enter`, `expect "› /model mock/small"`, `key enter`, `wait "model → mock/small"`, `expect "32.8k ctx"`, `type hi`, `key enter`. Expect: one row under the input, the id completed, then the switch. `check.sh` reads the journal and `requests.jsonl`: head row `mock`, a `model` row `mock/small` with window 32768, and the one request sent as `mock/small`. Contract: `model_ids_complete_from_the_cached_list` in `crates/construct-dev/tests/slash.rs`.
- **Model.** Drive: `type /model verify-model-x`, `key enter`, `wait "model → verify-model-x"`, `expect-style "model → verify-model-x" fg=#7aa2f7`. Expect: the notice in blue and the status line showing `verify-model-x`.
- **Unknown.** Drive: `type /nope`, `key enter`, `expect-style "unknown command /nope" fg=#e0af68`, `image {evidence}/slash.png`. Expect: the yellow hint.
- **Memory, new, quit.** Drive: `type /memory`, `key enter`, `wait "memory is empty"`, `type /new`, `key enter`, `gone "unknown command /nope"`, `type /quit`, `key enter`, `gone "ask construct" 3000`. Expect: the memory notice, a cleared transcript, then construct exits.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/slash-commands`. Expect: `ok: no model requests` — `requests.jsonl` is empty.

Palette and skill run: `setup.sh` writes the `create-verification` skill to the scratch home. Model: one text reply `ran-it`. Recipe: `.agents/skills/verify-construct/recipes/slash-palette`.

- **Palette.** Drive: `type /`, `expect "more (↑↓ to scroll)"`, `type cre`, `wait "▸ /create-verification"`, `expect-style "/create-verification" fg=#9ece6a bold`, `image {evidence}/palette.png`. Expect: the skill alone, green, with its description.
- **Complete and run.** Drive: `key enter`, `expect "› /create-verification"`, `type do-the-thing`, `key enter`, `wait "ran-it"`. Expect: the reply.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/slash-palette`. Expect: `ok: request carried SKILL.md and do-the-thing; invoke row for create-verification` — the last `requests/req-*.json` holds the SKILL.md body and the request, and `journal.txt` has the `invoke` row.
- Contract: `crates/construct-dev/tests/slash.rs` (`listing_commands_draw_boxes`; a skill written by `skill_manage` in turn 1 runs from the palette in turn 2; built-ins from the palette; esc), golden `help_panel` in `snapshots.rs`.

## Gotchas

- A skill can't take a built-in's name or alias (`/clear`, `/exit`, `/q`, `/?`); the built-in wins, so pick a skill name for the palette step that no command starts with.
- Built-ins never reach the model: the first recipe's proof is that `requests.jsonl` is empty, so don't add a step that sends a turn to it.
