# 0070 — Joining a hub: invite tokens, walk-up approval by SAS code, live-only pending, revocation

Status: accepted. 2026-11-25. Part of #273, #361. Builds on ADR 0068, 0069;
amends 0069's admission rules and registry states.

## Context

ADR 0069 left the hub admitting any key that completed Noise IK + `Hello` as
a `pending` row and promoting nothing. #361 settles how a peer becomes
trusted and how it stops being trusted, under these constraints:

1. The hub's public key is in every stamped peer binary (#363), so it leaks
   with the first lost USB stick. Holding it must not let anyone raise
   prompts at the hub or guess a token.
2. Joining must not need typing on both machines at once: the operator has an
   SSH session open to the target (primary path), or walks to the target and
   back to the desk (fallback).
3. A known peer id must never change keys without the operator acting.
4. Revocation must hold even if the peer never hears about it.
5. `construct hub invite` and `revoke` should work when no hub is running,
   without breaking ADR 0069's rule that only the lock holder writes the
   registry.

## Decision

- **Peer key.** `construct peer` (the binary's `peer` feature) creates its
  X25519 key in `$CONSTRUCT_HOME/peer.key` on first run and reuses it.
  `construct_peer::keyfile` holds the rules for both `hub.key` and
  `peer.key`:
  - only the 32 private bytes are stored, mode `0600`;
  - a key file that group or others can read is refused;
  - the key is written whole to a private temp file and then hard-linked
    into place, so a reader never sees a half-written key and the first
    writer wins a race.
- **Primary path: `construct hub invite <id> [--expires 15m]`.**
  - It opens an `expected` slot holding a single-use token (16 random bytes,
    hex). The registry stores only the token's SHA-256 and expiry.
  - It prints one command for the target:
    `CONSTRUCT_JOIN=<token> construct peer --url … --hub-key … --id <id>`
    (`--join` works too). Stamping shortens this later.
  - The token travels only inside the encrypted `Hello`.
  - A matching, unexpired token pins the key on the spot, clears the invite
    and resets the strike count. Later connects need neither token nor
    approval.
- **Fallback path: walk-up approval.**
  - A peer without a token is held for a human only when either:
    - its id has an `expected` slot, or
    - `construct hub pair [--for 10m]` opened a pairing window.
  - The hub answers `Control::Pending`. Both sides compute the same
    `XXX-XXX` code from the Noise handshake hash: the peer prints it, and
    the hub shows it in its log, in `hub status`, in `hub pending`, and (with
    #365) in the TUI.
  - `construct hub approve <id>` pins the key and sends
    `JoinAck { accepted: true }`. `deny` sends `JoinAck { accepted: false }`
    and closes. Nothing is typed on the peer.
- **Pending exists only while the link is open.**
  - Only `expected`, `pinned` and `revoked` are ever written to the file.
    `pending` is the hub's in-memory view of a link waiting for a human. The
    code exists only in that live handshake, so a dead connection leaves
    nothing to approve and no row to clog the cap.
  - Approving and denying therefore require the running hub.
  - At most 5 joins wait at once. When full, a newcomer is refused; no
    connected join is ever evicted.
  - Rows marked pending by an older hub load as empty `expected` slots.
- **Abuse rules,** all pure functions in `construct-hub/src/admit.rs`:
  - **Unknown ids.** Without a pairing window an unknown id is refused, with
    or without a token, and leaves no row. A leaked binary trying made-up
    ids gets no prompt and has no slot to strike against.
  - **Wrong tokens.** A wrong token on an `expected` slot adds a strike. The
    fifth strike burns the slot (the row is removed), and only a new
    `invite` makes another.
  - **Expired tokens.** An expired token is refused and its invite cleared.
  - **Known ids.** A pinned id arriving with another key is closed and its
    row is untouched. A second key waiting under an id that is already
    waiting is refused.
  - **Before `Hello`.** A replayed Noise message 1 never reaches admission
    (ADR 0068's 1-RTT `Hello`).
- **One verdict on the wire per join.** After `Hello` the hub sends exactly
  one of: `JoinAck { accepted: true }`, `Pending` (a `JoinAck` or `Revoke`
  follows), or `Close` with the reason.
  - The placeholder `Control::Join` is removed, since the token lives in
    `Hello`. `Control::Pending` is added.
  - These are peer-channel messages, outside the ACP contract.
- **Revocation: `construct hub revoke <id>`.**
  - The row becomes `revoked` and keeps its key, so that key gets no
    message 2 under any id (ADR 0069) and the id is refused with any new key.
  - A live link gets `Control::Revoke`, then `Close`. The peer deletes
    `peer.key` and exits non-zero.
  - It holds even if the peer is offline, because only the hub accepts keys.
  - `construct hub invite <id>` reopens a revoked id as a fresh `expected`
    slot: an explicit operator act.
- **Offline invite and revoke.** With no hub running, `invite` and `revoke`
  briefly take the hub's instance lock and edit the file under it. The
  one-writer rule holds, and an invite can be made before the first hub
  start. An offline invite prints `ws://<CONSTRUCT_HUB_LISTEN or
  127.0.0.1:9443>` unless `--url` is given.
- **`peer` joins `full`.** A host binary stamped for its own platform (#363
  defaults to `current_exe()`) must be able to dial, and the e2e suite drives
  real `construct peer` processes. `lean` and bare stay without it. Stamped
  drops are `bare` + `peer`, as before.

## Consequences

- #362 runs ACP only on links that received `JoinAck { accepted: true }`.
  #365's popup reads `code` and `remote` from the status the hub already
  serves and sends `approve` or `deny` over the socket.
- A hub restart drops waiting joins; the peer reconnects (#367) and shows a
  new code. Pinned peers are unaffected.
- A burnt or revoked slot needs the operator to run `invite`. An attacker can
  burn a slot whose id they know (5 bad tokens), which costs the operator one
  `invite`. They cannot get a key pinned that way.
- The token is visible in the operator's shell history and the target's
  environment for its lifetime (15 minutes by default, single use).
- Nothing yet re-sends a lost token or rotates a peer key in place.
  Rotation is revoke, then invite.

## Evidence

- e2e `crates/construct-dev/tests/join.rs`, with real `construct hub` and
  `construct peer` processes, separate homes, on loopback:
  - **Invite:** the printed command is exactly
    `CONSTRUCT_JOIN=<token> construct peer --url <hub url> --hub-key <hub key>
    --id old-pc`. The peer creates `peer.key` with mode `0600`, joins with
    the token, and is pinned to that key with the invite spent. A second run
    without a token reuses the key and joins.
  - **Walk-up approval:** a peer without a token on an invited slot prints a
    code. The same code appears in `hub status` and `hub pending`. `hub
    approve` pins the peer's key while it waits. A pairing-window join
    turned away with `hub deny` exits non-zero ("denied at the hub") and
    leaves no row.
  - **Tokens:** four wrong tokens are each refused and counted. The fifth
    burns the slot, after which even the real token is refused ("no
    invite"). An invite with `--expires 1s`, used after 2 s, is refused as
    expired.
  - **Leaked key:** ten made-up ids, with and without tokens, are each
    closed ("not expected" or "no invite") and the registry stays empty.
    With a pairing window open, five joins wait and the sixth is refused
    ("too many").
  - **Revoke:** a connected peer stops with a non-zero exit and deletes
    `peer.key`. Running it again with its new key is refused ("was
    revoked"). The old key under a new id gets no handshake.
- e2e `hub.rs` (from ADR 0069), updated: an unknown peer waits only inside a
  pairing window and shows a 7-character code. A pinned id with another key
  is still closed. After `hub stop` the waiting join is gone and the pinned
  row remains. `invite` and `revoke` work with no hub; `approve` refuses.
- Unit tests: admission in `admit.rs` (valid, spent, expired and wrong
  tokens, the burn, unknown ids, pairing, the cap, a second key for a
  waiting id, known ids, revoked ids, bad ids); transitions in
  `registry.rs`; tokens and spans in `invite.rs`; `keyfile` (mode, stable,
  delete, loose-key refusal); `random_token` in `construct-peer`.
- A bug found on the way: an e2e run caught `hub status` reading a half-written
  `hub.key` (0 bytes) while the hub was creating it. The temp-file + hard-link
  write fixes it, and the same e2e passed twice in a row afterwards.
- Release binary size (`[profile.release]`, `arm64-apple-darwin`), this
  branch on `origin/main` at `6969e62`:

| build | bytes | vs. before |
|---|---|---|
| `full` (now with `peer`) | 8,660,832 | +83,616 B (+0.97%) vs. ADR 0069's `full` (8,577,216) |
| `full` without `peer` | 8,627,152 | so `peer` costs `full` +33,680 B (+0.39%) |
| `lean` | 6,339,632 | unchanged |
| bare (`--no-default-features`) | 5,792,752 | unchanged |
| bare + `peer` (a stamped drop) | 5,926,064 | +112 B vs. ADR 0068's 5,925,952 |

  `construct manifest` lists `peer` and `hub` in `full`, `peer` alone in
  bare + `peer`, and neither in `lean` or bare. Neither `lean` nor bare
  contains the hub's or the join's strings, and bare + `peer` contains the
  join's but not the hub's.
