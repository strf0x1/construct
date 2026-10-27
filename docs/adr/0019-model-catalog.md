# 0019 — A hand-written model catalog, laid over the defaults

Status: accepted. 2026-10-26.

## Context

#69: switching models meant knowing the exact id (`/model <id>`, no list), and
a model's limits lived only in global config. After `/model` to a 1M-window or
a 32k model, compaction kept planning for 131k, and the `ctx` meter and the cost
figure were wrong in the same way. An agent definition (`[agents.x] model`) had
to repeat `context_window` to get its own model's window.

## Decision

- **A catalog the user writes.** `$CONSTRUCT_HOME/models.toml` and `[models.*]`
  in `config.toml`, merged, `config.toml` winning a clash. One table per model
  id: `label`, `description`, `tags`, `context_window`, `max_tokens`,
  `base_url`, `api_key_secret`, `dialect`, `cache`, `pricing` (USD per million
  tokens). Field names follow OpenRouter's `/models` (`context_length`,
  `top_provider.max_completion_tokens`, `pricing`) so a later auto-fill is a
  mapping; nothing polls it, and no dependency was added (convention 8).
  Unknown keys are refused, so a typo in a hand-written file is named instead
  of silently ignored.
- **Entries are laid over the defaults, never a replacement for them.** The
  top-level `context_window`, `max_tokens`, endpoint and provider stay the
  defaults. `Config::select_model(id)` restores them, then applies the entry
  if there is one, so a switch always lands on "defaults plus that entry" and
  an id with no entry gets the defaults again. Everything downstream (the
  compaction budget, the `ctx` meter, requests, the manifest) keeps reading
  `cfg.context_window` and `cfg.max_tokens` and is right without knowing the
  catalog exists. Every place that chose a model (`--model`, `CONSTRUCT_MODEL`,
  `/model`, `[agents.x] model`) goes through `select_model`.
- **Precedence for a child:** its definition's own `context_window`, then its
  model's entry, then the parent's value.
- **A different endpoint takes its own key.** An entry that sets `base_url`
  drops the extra `headers`, resets the dialect, and uses only its own
  `api_key_secret` (none when absent). `Config::endpoint_key` also skips the
  OpenRouter-name fallback once the endpoint has moved, so a credential found
  for one host is never sent to another. This is the rule `[agents.x]` already
  followed for `base_url` (ADR 0014).
- **The journal records it.** The head row's manifest holds the window and
  output cap the session started with (`max_tokens` is a new defaulted field);
  a switch is an `Event::Model` row (`model`, `context_window`, `max_tokens`),
  additive per ADR 0008. The TUI draws its notice and meter from that event,
  not from its own guess, so a switch that fails (a key that does not resolve)
  changes nothing on screen either.
- **`/model` with no argument opens a picker** over the catalog (`tui/picker/`):
  a box over the transcript, a search line, a table with window and price, the
  highlighted model's details beside it. The running model is ticked and
  selected; picking it closes the box. `/model <id>` still takes any id.
- **`pricing` fills in cost only when the provider reports none.**

## Consequences

- The ids the user typed today keep working. A catalog entry is opt-in per
  model, and a missing `models.toml` changes nothing.
- A provider-stated window (`window_seen`, from an overflow error) describes the
  model that sent it and is dropped on a switch.
- Delegation's `Delegate` is built once per agent, with the model the session
  started on. A `/model` switch does not reach a definition that names no model;
  that was already true and is not changed here.
- Mid-session switching rebuilds the HTTP client only when the endpoint, key or
  dialect changed, so a switch between models on one endpoint reuses the warm
  connection.
- Revisit if the catalog is ever filled from OpenRouter: `deny_unknown_fields`
  will need to move to the hand-written entry only.

## Evidence

- #69. Tests: `crates/construct-dev/tests/catalog.rs` (the picker lists the
  catalog and a choice reaches the next request; compaction at a 16k entry's
  window against an endpoint that enforces 16k, with an id-with-no-entry
  control), unit tests in `models.rs`, `agents.rs` and `tui/picker/`.
- `verify-construct` recipe `model-picker`.
