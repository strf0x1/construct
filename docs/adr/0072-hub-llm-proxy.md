# 0072 — Hub LLM proxy: `llm = "hub"`, a loopback gateway on the peer, keys only on the host

Status: accepted. 2026-11-25. Part of #273, #371. Builds on ADR 0027, 0051,
0068, 0069, 0070, 0071.

## Context

A peer (ADR 0070, 0071) runs its own agent loop, so it makes its own model
calls. Until now that meant an API key in the target's environment (ADR
0051): a dropped binary on a strange box holding a credential that can spend
money, with no way to cut it off but rotating the key. #371 moves the key
back to the host. Constraints:

1. Every model call a process makes must go the same way: the turn, the
   compaction summary, the learning pass, a `/model` switch. There are a
   dozen `llm::Client::new` sites, all fed by `Config::endpoint_for`.
2. The client's behaviour (retries on 429/5xx, `Retry-After`, the first-byte
   and idle deadlines, Deck denials, the SSE parser, `notes`) is tested and
   must not fork.
3. The peer wire already reserves an `llm` channel (ADR 0068). Envelopes stay
   outside the ACP contract.
4. A link lost mid-stream (laptop sleeps, hub killed) must fail the call, not
   look like a short, complete answer, and not wait out a 120 s deadline.
5. Revocation must end model access at once, including calls in flight.

## Decision

- **`llm` is a config key**: `local` (default, unchanged) or `hub`
  (`config/llm.rs`). It is serialized only when `hub`, so existing configs
  hash as before. It means something only to `construct peer`; `run`, the
  TUI and `acp` ignore it.
- **The peer gets a loopback gateway, and the engine is not told about the
  hub.** With `llm = "hub"`, `construct peer` binds
  `construct_peer::gateway` on `127.0.0.1:0` and sets the runtime-only
  `Config::llm_gateway`. While it is set, `Config::endpoint_for` returns one
  endpoint for every selector (`hub`, the gateway's URL, no key, the openai
  dialect, this config's deadlines) and `wire_id` sends the selector as
  written. Every `Client` in the process, wherever it is built, goes there,
  and the client code is unchanged.
- **The gateway takes a random bearer token** that exists only in the peer
  process. Sandboxed tools get a credential-free environment, so a shell
  command on the peer cannot spend the host's key through the loopback port.
  It serves only `POST /v1/chat/completions` with a `Content-Length` body.
- **The `llm` channel relays bytes, not meaning** (`construct_peer::llm`).
  A frame is a JSON header line, `\n`, then raw bytes. Peer to hub:
  `request {id}` + the body, `cancel {id}`. Hub to peer: `head {id, status,
  retry_after}`, `data {id}` + a slice of the provider's body, `end {id,
  error}`. The peer's client therefore reads the provider's own status and
  stream, and its retries, deadlines and parser apply as they would direct.
- **The response is chunked on the loopback**, so a lost link closes the
  connection before the last chunk and the client fails with "model stream
  interrupted". With no link at all, a call is answered 503 at once, which
  the client retries with backoff while the peer redials.
- **The credential boundary is `construct-hub/src/proxy.rs`.** For each
  request the hub resolves the body's model selector against its own config
  (`endpoint_for`, `wire_id`, ADR 0027), takes the key from its own
  environment or keyring (`Endpoint::require`, ADR 0051), shapes the body
  for that provider (`Provider::shape`), and relays status and bytes back.
  Nothing the hub resolved reaches the peer but the response. A missing key
  is a 403 that names the secret on the host; a transport error is a 502.
- **Calls belong to a link.** Each link's `Calls` aborts its calls on
  `cancel` (the peer's caller hung up) and all of them when the link ends:
  a revocation, a drop, a hub shutdown. A request is served only while the
  peer's key is pinned and its link is not waiting for approval; otherwise
  it is a 403. At most 4 calls per peer run at once; a fifth is 429 with
  `Retry-After: 1`.
- **Usage is attributed per peer while the hub runs**: requests, prompt and
  completion tokens, and cost, read from the `usage` object of the relayed
  stream, shown in `construct hub status` (`llm` on the peer's row). It is
  not persisted.
- **`llm = "local"` stays first-class** for a box that must finish turns
  offline: nothing about it changes.

## Consequences

- No API key needs to sit on a target. Changing provider, key or a model's
  route happens in the host's config; a peer's model selector is resolved
  there, so a selector the host does not know is sent upstream as written.
- The peer's own catalog still sizes its context window from its own model
  entry. Discovery and caching of the host's models on the peer are out of
  scope (#371).
- Hub-proxied turns stop when the hub is unreachable: the peer's client
  retries 503s with its usual backoff, then the turn fails. Local-key peers
  keep working through an outage (ADR 0071's detach still holds for them).
- Revocation cuts model access in two places: the link (and its calls) ends
  at once, and a revoked key never completes a handshake again.
- The stamp (#363) defaults to `llm = "hub"`; the coordinator's `peer`
  tool (#366) and the TUI (#365) can read usage from the status.
- A request body is capped by one pinned envelope (16 MiB less a header).
- Not measured here: release binary size. The peer gains a ~350-line (non-test)
  gateway and codec on crates it already links (tokio, serde_json); the hub
  gains `proxy.rs` on reqwest, which core already links.

## Amendment — which model a peer uses (2026-11-25)

- **Stamping sets the default.** `construct build` (#363) bakes the peer's
  `model` selector alongside `llm = "hub"`; the hub resolves it against the
  host's config on every call, as above.
- **A running peer's model can be switched at will, from the hub.** Because
  the peer sends its selector as written and only the hub resolves it, the
  switch lives at the hub: a per-peer selector override the proxy applies
  before `endpoint_for`/`wire_id`, set over the control socket (a
  `construct hub model <peer> <selector>` op, and the TUI/`peer` tool as its
  clients, #365, #366). No ACP change is needed, and the override survives
  the peer redialing. Not built in #371: ACP has no model-switch method
  today, and a peer's context window still comes from its own catalog entry
  for the stamped selector, so the override must either carry a window or
  stay within models of a similar size until that is settled.
- **Usage counts reset with the hub** by design; nothing persists them.
- **Built in #390, ADR 0078**: the override is `construct hub model`, kept
  on the peer's registry row, and the window question is settled there. The
  hub refuses a request too big for the override's window, phrased as the
  provider overflow the peer already recovers from.

## Evidence

- e2e `crates/construct-dev/tests/peer_llm.rs`: real `construct hub` and
  `construct peer`, the peer in its own home with no `OPENAI_API_KEY` and
  `CONSTRUCT_BASE_URL` pointing at a port nothing serves.
  - `a_peer_without_a_key_runs_a_tool_turn_through_the_hub_and_revoking_it_ends_its_model_access`:
    a shell call and a text answer, two model requests, both with the hub's
    key at the scripted model; the file the tool wrote exists; the second
    request carries the tool result; `hub status` counts 2 requests for the
    peer. `hub revoke` stops the peer (non-zero, "revoked") and no further
    request reaches the model.
  - `a_hub_killed_mid_stream_fails_the_peers_model_call_instead_of_leaving_it_hanging`:
    the model sends words then stalls for 60 s; SIGKILL on the hub; the
    peer's journal shows an `error` row ("model stream interrupted: …
    unexpected EOF during chunk size line") 6 ms after the kill.
  - `a_local_peer_calls_its_own_endpoint_with_its_own_key`: with the hub's
    endpoint dead, a default peer's request reaches the scripted model with
    the peer's own key, and the hub records no usage for it.
- Mutations: ending the gateway's body cleanly on a lost link (writing the
  last chunk) makes the kill test fail (no error, the turn ends on a
  truncated answer); dropping the gateway override in `endpoint_for` makes
  the first test fail (the turn never ends).
- Unit tests: `construct-peer` `llm.rs` (frames round-trip, raw bytes with
  newlines; a frame without a header is refused), `gateway.rs` (no link, no
  call; unlinking cuts every call and sends no cancel; a cancel reaches the
  hub once; an end retires a call); `construct-hub` `proxy.rs` (usage from
  the last SSE chunk across splits; not pinned is 403 with nothing upstream;
  a non-JSON body is 400; a fifth call in flight is 429 and a cancel frees
  the slot).
