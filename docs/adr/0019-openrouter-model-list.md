# 0019 — OpenRouter's model list fills the context window; the config wins

Status: accepted, 2026-10-26.

## Context

`context_window` defaulted to 131072 whatever the model was, and compaction
plans against it. OpenRouter serves hundreds of models with windows from 8k to
1M, and its public `GET /api/v1/models` states each one's `context_length`,
prices and `supported_parameters`. oh-my-pi reads the same endpoint, but
behind a SQLite cache, a generated 13 MB bundled catalog and per-provider merge
policies; most of that is for being multi-provider.

## Decision

1. **`llm::catalog` fetches `{base_url}/models`** with no key (the list is
   public), a 10 s timeout, and parses the fields it needs (`parse` is pure).
2. **The cache is one JSON file per endpoint** under `$CONSTRUCT_HOME/models/`,
   2 h TTL. It holds the whole list, so a `/model` switch is a file read. A
   failed fetch falls back to a stale cache; with neither, nothing changes.
3. **Only the OpenRouter dialect fills anything in.** Other endpoints do not
   state windows, so `construct models` and `/models` list their ids and
   nothing else touches their config.
4. **The config file wins.** `context_window` set in `config.toml` (or by an
   agent definition) is never replaced (`Config::window_pinned`). An unlisted
   model gets the default again, not the previous model's window.
5. **Startup waits at most 3 s** for a list, in the agent task and before a
   headless run, never on the UI path. The UI only reads the cache.
6. **Children and ACP do not look anything up.** A child inherits the fitted
   window through its serialized config.

## Consequences

Compaction and the context percentage in the status line match the model on
OpenRouter without any config. A cold cache and no network costs 3 s before
the first turn. Prices are parsed but only shown by `/models`; using them for
cost estimates, and `max_tokens` from `top_provider`, are open. Tab-completing
a model id in the slash palette is open.

## Evidence

`crates/construct/src/llm/catalog.rs` unit tests,
`crates/construct-dev/tests/models.rs`, and the `models` verify recipe.
