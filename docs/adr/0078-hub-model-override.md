# 0078 — Switching a running peer's model from the hub, and the window that comes with it

Status: accepted. 2026-11-27. Part of #390. Builds on ADR 0027, 0072, 0074,
0075.

## Context

A peer with `llm = "hub"` sends the model selector it was stamped with, and
only the hub resolves it to an endpoint, key and dialect (ADR 0072). Its
amendment says a running peer's model is switched at the hub, by a per-peer
override the proxy applies before `endpoint_for`/`wire_id`. It left one
question open: the peer sizes its context window, and so its compaction
budget, from its own catalog entry for the stamped selector. An override to
a model with a smaller window would overflow it. Three options were on the
table (#390):

1. The override carries a window and the hub sends it to the peer. That is a
   new control or ACP signal, and an ACP change starts in
   `sprawlcommons-acp`.
2. Only allow overrides to models whose window is at least the stamped one.
   The hub cannot check this. The peer's window comes from the peer's
   catalog (often just the default), not from the host's, and the hub never
   sees it.
3. The proxy clamps or refuses requests over the override's window.

The engine already recovers from an overflow. When a provider answers "This
model's maximum context length is N tokens", the agent compacts and keeps
`N` as the window for the rest of the session (`agent.rs`,
`compact::overflow_limit`).

## Decision

- **The override is a registry field.** `Peer::model` on the peer's row in
  `hub/peers.json` is saved with the row, so it outlasts a redial and a hub
  restart. Revoking the peer clears it. The link reads it on every model
  call, so the next call after a change uses it. Nothing is sent to the peer.
- **The proxy applies it first.** In `proxy.rs` the override, when set,
  replaces the body's `model` before `endpoint_for`, `wire_id` and the key
  are resolved. The selector the peer sent is still recorded (in memory) as
  `asked_model`.
- **The window: option 3, as an overflow the peer already reads.** When an
  override is set and the host's config gives its model a window (any layer
  but the built-in default), a request whose body is more than that window
  at 4 bytes a token is answered `400` with
  `code: context_length_exceeded` and "This model's maximum context length
  is N tokens. However, your messages resulted in about M tokens". Nothing
  goes upstream. The peer compacts to `N` and keeps it. 4 bytes a token
  underestimates JSON and code, so the hub refuses only what cannot fit and
  leaves the precise check to the provider, whose own rejection the peer
  handles the same way. "about" keeps the estimate out of the peer's
  bytes-per-token measure (`overflow_tokens` reads only a number right after
  "resulted in "). With no window known on the host, the hub checks nothing.
- **Clients.** It is set over the control socket's `model` op
  (`{"op":"model","id":…,"model":…}`; no `model` clears it), answered with
  the status:
  - `construct hub model <peer> [<selector> | --clear]`; with neither, it
    prints the effective model. With no hub running, the registry is edited
    under the instance lock, as `invite` and `revoke` are.
  - The TUI: `/peer <name> --model [<selector> | --clear]`.

  The selector must resolve in the host's config (an ambiguous one is
  refused). A selector the host does not catalogue is sent upstream as
  written, as ADR 0072 already does.
- **What it shows.** Each status row carries `model` (the override) and
  `asked_model` (what the peer last sent). `construct hub status` prints
  `model X (set at the hub)` or `model X`. The TUI's tile shows the model's
  last path segment, in lavender with `(hub)` when it is an override.
- **The coordinator's `peer` tool does not switch models.** Picking a model
  is a spending decision, and ADR 0075 keeps the tool away from anything the
  user decides. It stays at `list`, `send`, `status` and `cancel`.

## Consequences

- A model switch needs no ACP change and no restart: no new wire on either
  side, and peers already in the field need no update.
- A peer switched to a smaller model loses context once (one compaction),
  then runs inside the smaller window. The window it learned stays for the
  rest of that ACP session even after the override is cleared. That wastes
  some room, but nothing fails. A new session starts from the catalog again.
- A peer switched to a larger model keeps its own, smaller window. That is
  safe; to use the larger window, change the peer's catalog.
- Not done: the peer tool showing the model in `list`; a size check that
  counts the request's `max_tokens` as well (the provider still checks
  that).
- Revisit if ACP gains a model or window signal (contract first), which
  would let the hub tell the peer its window before the first call.

## Evidence

- e2e `crates/construct-dev/tests/peer_llm.rs`, with a real `construct hub`
  and `construct peer` (peer stamped `model = "big"`, `llm = "hub"`, no key):
  - `a_model_set_at_the_hub_takes_the_peers_next_call_and_clearing_it_gives_the_peers_own_back`:
    `hub model old-pc small` keeps `model: small` on the row. The scripted
    model receives `model: "small"`. Status shows `asked_model: big`. After
    `--clear`, the same running peer's next request is `model: "big"`.
  - `an_override_too_small_for_the_peers_context_is_refused_at_the_hub_as_an_overflow_the_peer_reads`:
    with `[models.tiny] context_window = 50`, no request reaches the model.
    The peer's only error is its overflow recovery's own ("the prompt is too
    long and nothing is left to cut"), not a bare 400.
  - `a_model_is_set_with_no_hub_running_and_a_running_hub_reports_it`.
- Mutations: ignoring the override in `Calls::request` fails the first test
  (the model receives `big`); skipping the window check fails the second (a
  request reaches the model).
- Unit tests: `construct-hub` `model.rs` (the rejection passes core's
  `is_overflow` and `overflow_limit` and gives `overflow_tokens` nothing; a
  window is known only when the host's config sets one), `registry.rs` (the
  override survives a reload; a revoked peer takes none), `proxy.rs` (an
  oversized body is refused 400 `context_length_exceeded` before anything
  goes upstream, and the same body without the override goes upstream),
  `status.rs` (rendering); `construct-tui` `tile.rs` (short name,
  `(hub)` marker).
