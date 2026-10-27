# 0020 — OpenRouter's model list is laid under the hand-written catalog

Status: accepted, 2026-10-26. Builds on 0019, whose catalog anticipated this
("a later auto-fill is a mapping").

## Context

0019 lets the user write a model catalog, but on OpenRouter that means
hand-copying windows and prices for hundreds of models. OpenRouter's public
`GET /api/v1/models` states each model's `context_length`, prices and
`supported_parameters`. oh-my-pi reads the same endpoint, behind a SQLite
cache, a generated 13 MB bundled catalog and per-provider merge policies; most
of that is for being multi-provider.

## Decision

1. **`llm::catalog` fetches `{base_url}/models`** with no key (the list is
   public), a 10 s timeout, and parses the fields it needs (`parse` is pure).
2. **The list becomes `Config::discovered`, never `Config::models`.** Entries
   are built from it (`label`, `tags` reasoning/vision, `context_window`,
   `pricing`) and `Config::lookup` consults the user's catalog first, then the
   list (a `vendor/model:variant` id takes `vendor/model`'s entry). Everything
   downstream is 0019's: `select_model`, the journal's `Event::Model`, the
   picker, a child's model. `discovered` is `#[serde(skip)]`, so
   `construct config` and the manifest hash do not carry hundreds of rows.
3. **The user wins.** A `[models.<id>]` entry beats the list for that id. A
   top-level `context_window` in `config.toml` is kept (`Config::window_pinned`):
   list entries then carry no window. The list never sets `max_tokens`.
4. **The cache is one JSON file per endpoint** under `$CONSTRUCT_HOME/models/`,
   2 h TTL; a failed fetch falls back to a stale cache; with neither, nothing
   changes. `overlay` reads the cache only, so the UI and a model switch never
   make a request.
5. **Only the OpenRouter dialect overlays anything.** Other endpoints do not
   state windows; `construct models` and `/models` still list their ids.
   Only tool-capable models are overlaid (an agent needs tools).
6. **Startup waits at most 3 s** for a list, in the agent task and before a
   headless run, never on the UI path. ACP does no lookup.

## Consequences

On OpenRouter, compaction, the `ctx` meter, the cost fallback and the picker
match the model with no config; the picker lists the whole tool-capable list
after the user's own entries. A cold cache and no network costs 3 s before the
first turn. Using `top_provider.max_completion_tokens` as an output cap is open
(it changes request bodies, so it wants a measurement). 0019's note about
`deny_unknown_fields` still stands: the list is built in code, not parsed into
`ModelEntry`.

## Evidence

`crates/construct/src/llm/catalog.rs` unit tests,
`crates/construct-dev/tests/models.rs`, and the `models` verify recipe.

## Note: model ids in the palette

The slash palette completes model ids inline: after `/model ` and a word it
lists the catalog (`Config::models`, then `Config::discovered`) with the
picker's matching and `Row::of` for each window, and enter completes
`/model <id>`. It reads nothing from disk and fetches nothing; the list is
whatever `overlay` laid under the catalog. Bare `/model ` still opens the picker.
