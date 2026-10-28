---
id: model-picker
status: built
surfaces: [tui]
---
# Model picker

`/model` with no argument opens a box over the transcript listing the models in the user's catalog (`models.toml` in the construct home, or `[models.*]` in `config.toml`). The running model is ticked and selected. Typing narrows the list (id first, then label, tags and description); the highlighted model's limits are shown beside it: context window, where compaction starts, max output, price, endpoint, dialect and key name. Enter switches (the box closes, a notice says `model → <id> (<label>) · <window> ctx`, the status line and `ctx` meter follow, the next request carries the new id and the new window is what compaction plans against); esc closes it and changes nothing. `/model <id>` still switches to any id; one with no entry runs on the top-level `context_window` and `max_tokens`. With no catalog, `/model` prints how to start one.

## Sub-features

- `picker-open` `/model` + enter (it is also in the `/` palette) shows the box: `◆ models` and a count on the border, `⌕ search models…`, a `Model / Ctx / $/M in·out` table, `▸` on the highlighted row, a green `✓` on the running model. At 86+ columns the details sit beside the list; narrower, the list alone.
- `picker-filter` typing narrows (`1/4` on the border when one of four matches); `no model matches` when none do. ctrl+u clears the query. ↑↓, ctrl+p/n, pgup/pgdn, home/end move.
- `picker-switch` enter on another model sends it; on the running one it only closes the box.
- `picker-limits` the entry's `context_window`, `max_tokens`, `base_url`, `api_key_secret`, `dialect` and `pricing` replace the defaults for that model; the journal `head` row holds the starting window, and each switch is a `model` row.

## How to get to it (user POV)

- Put entries in `~/.construct/models.toml` (`[models."anthropic/claude-haiku-5.5"]` with `label`, `context_window`, `max_tokens`, optional `pricing = { prompt = 1.0, completion = 5.0 }` per million tokens), then type `/model` in the TUI.

## Done when

- The box lists every catalog entry; choosing one changes the status line and the next request's `model`; the `model` journal row holds that entry's window and cap; compaction plans against the entry's window.

## Driving it

Preconditions:

- Baseline state plus the recipe's `setup.sh`, which writes a `config.toml` (default model `anthropic/claude-haiku-5.5`, local router, learning off) and a four-model `models.toml`. Recipe: `.agents/skills/verify-construct/recipes/model-picker`.

- **Open.** Drive: `type /model`, `key enter`, `wait "search models…"`, `expect "anthropic/claude-opus-5.5"`, `expect "Compacts at"`, `expect-style "✓" fg=#9ece6a`, `image {evidence}/picker.png`. Expect: four rows, the running model ticked, its details on the right.
- **Filter and switch.** Drive: `type opus`, `wait "1/4"`, `gone "mini/tiny-8k"`, `key enter`, `wait "model → anthropic/claude-opus-5.5"`, `expect "1M ctx"`. Expect: the box closed and the notice.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/model-picker`. Expect: `ok: request model opus; model row 1M/32000; head row manifest 200000` — the last request's `model`, the `model` journal row and the `head` row's manifest agree with what the screen said.
- Contract: `crates/construct-dev/tests/catalog.rs`.

## Gotchas

- With no catalog `/model` prints how to start one instead of opening the box; the recipe's `setup.sh` writes `models.toml` into the scratch home for that reason.
