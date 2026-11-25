# 0068 — Peer wire protocol: Noise IK, 1-RTT Hello, chunked envelopes, WebSocket

Status: accepted. 2026-11-25. Part of #273, #368.

## Context

Construct-to-construct peers (#273) dial home from remote or headless machines
over whatever lane reaches the host — first LAN/Tailscale (`ws://` or `wss://`),
later outside-NAT tunnels (`wss://`) — without exposing a target to anyone else
and without duplicating TLS or crypto stacks in the drop binary (`bare` / `lean`,
ADR 0064).

Three design questions had to be settled before building `construct hub` (#360)
and the join flow (#361):

1. **Transport & TLS:** Raw TCP fails through Cloudflare Tunnel, Tailscale
   Serve/Funnel, and reverse proxies; HTTP/2 streaming adds unnecessary framing.
   WebSocket binary frames (`tokio-tungstenite`) traverse every HTTP/HTTPS
   middlebox and map 1-to-1 to Noise messages. `wss://` must reuse
   `construct_core::http`'s ring + platform-verifier + `CONSTRUCT_CA_FILE` +
   musl baked-root policy (ADR 0053) rather than adding a second trust store.
2. **Channel crypto & replay defense:** `Noise_IK_25519_ChaChaPoly_SHA256`
   authenticates both static X25519 keys in one round trip and hides the
   initiator's static key from passive observers while dropping unauthenticated
   scanners on message 1 AEAD failure. However, Noise IK message 1 (`-> e, es,
   s, ss`) has no replay protection on its own, and the responder does not know
   the initiator holds `e` until the first valid transport message after
   message 2 (`<- e, ee, se`).
3. **Crypto backend & binary size:** `snow`'s `default-resolver` pulls in
   `aes-gcm`, `blake2`, `chacha20poly1305`, and `getrandom` 0.3 on top of the
   `ring` already linked for `rustls`. `snow`'s `RingResolver` handles AEAD,
   SHA-256, and CSPRNG via `ring`, but leaves `resolve_dh` unimplemented
   because `ring::agreement` is ephemeral-only.

## Decision

- **Dedicated crate (`crates/construct-peer`):** Owns the wire protocol shared
  by both sides, with zero dependency on `construct-tui` (enforced in
  `docs/architecture.toml`). Split into two additive features: `dial` (peer
  dialer; wired into the `construct` binary behind `--features peer`,
  independent of `hub`) and `accept` (server WebSocket upgrade for `construct
  hub`).
- **Stream abstraction (`FrameStream`):** Noise runs over a discrete-frame
  trait. Production uses `WsFrames` (`tokio-tungstenite` binary messages with
  `max_message_size` and `max_frame_size` capped at 65,535 bytes); tests use
  `LengthPrefixed<DuplexStream>` with no sockets or models.
- **Shared TLS trust policy (`construct_core::http::tls_client_config`):**
  Non-HTTP TLS clients (`wss://` dialing) build their `rustls::ClientConfig`
  from `construct_core::http`, inheriting ring, `rustls-platform-verifier`,
  `CONSTRUCT_CA_FILE`, baked musl roots, and the actionable empty-store error
  from ADR 0053.
- **Noise IK + mandatory 1-RTT `Hello`:** Pattern
  `Noise_IK_25519_ChaChaPoly_SHA256` with prologue `construct-peer-v1`.
  `snow` is compiled with `default-features = false, features = ["ring-resolver"]`
  plus a thin `PeerCryptoResolver` that delegates AEAD, SHA-256, and CSPRNG to
  `RingResolver` and static X25519 DH to `curve25519-dalek` (`MontgomeryPoint`).
  After Noise IK message 2, the initiator immediately sends an encrypted
  `Control::Hello` carrying `{version, peer_id, join_token?}`. The responder
  silently drops connections whose message 1 fails AEAD and refuses to expose
  any peer until `Control::Hello` decrypts and parses. The 32-byte Noise
  handshake hash (`h`) and deterministic `XXX-XXX` SAS code are exposed on
  `PeerLink` for #361.
- **Chunked envelopes (`{channel, bytes}`):** Logical channels `Acp = 1`,
  `Control = 2`, and `Llm = 3` (#371) are split across Noise's 65,535-byte
  frame ceiling (65,519 B plaintext) with a 10-byte header
  `[channel: u8][flags: u8][seq: u32 BE][total_len: u32 BE]`. Reassembly
  enforces `PRE_PIN_MAX_ENVELOPE` (4 KiB) prior to pinning (including on the
  initial `Hello`) and `PINNED_MAX_ENVELOPE` (16 MiB) once pinned, rejecting
  oversized `declared` lengths before buffering. Envelopes stay outside the ACP
  contract (`sprawlcommons-acp`).
- **Keep-alive and reconnect backoff:** Encrypted `Control::Ping`/`Pong`
  probes (default 25 s interval, 10 s pong deadline) detect dead links through
  middleboxes; `Backoff` applies capped exponential delay with ±25% jitter
  (default 500 ms → 30 s) and aborts immediately on `WireError::Rejected`.

## Consequences

- `bare` builds without `peer` pay zero bytes for the peer wire crate (the
  shared `tls_client_config` helper in `construct-core` reuses crates already
  linked by `reqwest`).
- Stamped `bare + peer` drop binaries gain ~133 KB (+2.3%) for the full
  WebSocket + TLS + Noise IK + X25519 + chunked envelope + keep-alive dialer.
- `snow` 0.10's `ring-resolver` still resolves `blake2` and `getrandom` 0.3 in
  Cargo's feature graph via snow's `ring-resolver` -> `std` feature chain; LTO
  strips unused hash symbols from the release binary, leaving the measured
  delta small. Revisit if a future `snow` release decouples `std` from
  optional crates.
- `Control` already reserves `Join`, `JoinAck`, and `Revoke` variants so #361
  extends the join state machine without changing envelope framing.

## Evidence

- Contract tests: `crates/construct-peer/tests/wire_contract.rs` (12 tests) +
  unit tests (6 tests): wrong static key silent drop, replayed msg 1 blocked by
  1-RTT `Hello`, responder key rejection, tampered frame AEAD failure, >64 KiB
  (192 KiB) chunked round-trips on `Acp`/`Control`/`Llm`, oversized pre-pin
  and oversized `Hello` rejection, keep-alive ping/pong and timeout, reconnect
  with backoff across dropped connections, and loopback `ws://` and `wss://`
  (ephemeral CA + rustls).
- Release binary size (`[profile.release]`: `opt-level = "z"`, fat LTO,
  `strip`, `panic = "abort"`), `arm64-apple-darwin`, against `origin/main` at
  `a45f98e`:

| build | bytes | Δ vs baseline `bare` |
|---|---|---|
| `bare` baseline (`origin/main` at `a45f98e`) | 5,792,768 | — |
| `bare` (after `http::tls_client_config`) | 5,792,752 | −16 B (0.0%) |
| `bare` + `peer` (`--no-default-features --features peer`) | 5,925,952 | **+133,184 B (+2.30%)** |
