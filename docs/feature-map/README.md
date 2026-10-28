# construct feature map

This directory is the maintained source for the user-facing behavior of construct. Read this file before driving construct, then use the matching feature file as the recipe. Format reference: the `feature-map-format` skill. The driver for every `tui` feature is the `verify-construct` skill (`.agents/skills/verify-construct/`).

## Surfaces

- `tui` — the full-screen interface: `construct` with no arguments in a terminal. Driven in a pty by `construct-dev drive` against a scripted model.
- `cli` — headless subcommands: `construct run "<prompt>"` (add `--json` for one event per line), `construct manifest`, `construct sessions …`.

## Baseline preconditions

- Build with `cargo build -p construct -p construct-dev`; `.agents/skills/verify-construct/scripts/doctor.sh` must print `OK` (binaries present, built from HEAD, no newer sources).
- Every run gets its own scratch `CONSTRUCT_HOME`, work directory and `outside/` directory, and its own scripted model endpoint (`scripts/mock.sh`); `.agents/skills/verify-construct/scripts/drive.sh <recipe-dir>` sets all of that up and tears it down.
- With no `config.toml` in the scratch home, the model shown is the default `openrouter/free`, the key is a dummy, and there are no agents, skills or memory.
- The TUI is ready when `ask construct` (the input placeholder) is on screen.
- Never drive an instance that was not started by this verification run, and never use the user's `~/.construct`.

## Driving conventions

- Start every recipe from the baseline state unless its preconditions say otherwise; a recipe's `setup.sh` adds files or `config.toml`.
- Handles are literal screen text (`PERMISSION`, `Allow once`, `ask construct`), key names (`enter`, `down`, `y`), and theme colors from `crates/construct/src/tui/theme.rs` for `expect-style`.
- Actions are drive-script lines (`type`, `key`, `wait`, `expect`, `expect-style`, `snap --style`, `image`), run by `construct-dev drive`.
- Model behavior is fixed by the recipe's `model.json`: one step per model request, parent and children alike, in request order.

## Proof and skip reporting

- Evidence goes to `.verify/evidence/<run-id>/<feature-id>/` (gitignored): `transcript.txt`, `requests/` (full request bodies), `requests.jsonl`, `journal.txt`, and any images.
- Capture the user action and the resulting state, not only the final screen: a styled snap or image of the state the user acts on, and of the state after.
- Prove effects from a second view: the tool result in the next request body, `access` rows in `journal.txt`, files in the work dir or `config.toml`.
- Record the feature ID, recipe and run id with every result.
- A `verified` entry lists in `depends:` the source paths its behavior lives in. When one of them, or a recipe it names, changes after `verified_at`, the `map` gate in `cargo xtask check` fails: re-drive the recipe at the commit that changed it and bump `verified_at` in a commit of its own, or set `status: built` (ADR 0023).
- Report an unreachable path with the attempted command and the unmet precondition.
- Do not report a skipped entry point as verified through a different path.

## Features

- [Permission prompt](./permission-prompt.md) — a tool reaching past the sandbox asks the user: once, for the session, always, or deny
- [Delegation](./delegation.md) — the model hands a task to a configured agent, whose own blocked access asks the user as a subagent
- [Slash commands](./slash-commands.md) — `/help`, `/skills`, `/model`, `/memory`, `/learn`, `/new`, `/quit`, `/<skill>`, and the `/` palette
- [Model picker](./model-picker.md) — `/model` opens the catalog: search, limits beside the list, switch
- [Scroll and copy](./scroll-copy.md) — wheel and keys scroll the transcript without mouse capture; `/copy` puts the last reply on the clipboard
- [Status line](./status-line.md) — the bottom row: model, cumulative in/out/cache, cost, context use, session id
