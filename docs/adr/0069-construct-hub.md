# 0069 — construct hub: one host process per home, a registry it alone writes, a 0600 control socket

Status: accepted. 2026-11-25. Part of #273, #360. Builds on ADR 0021, 0064, 0068.
Admission rules and the `pending` state amended by ADR 0070 (#361): unknown
ids now need an `expected` slot or a pairing window, and `pending` is live-only.

## Context

Peers (#273) dial home to something that holds the controller key, knows
which peer keys to trust, and keeps running when no TUI is open. Several
consumers need to reach it: TUIs (#365), `construct build` (#363), the
coordinator's `peer` tool (#366) and plain CLI commands. Questions to settle
before the join flow (#361) builds on it:

1. Where the hub's state lives and how it is protected.
2. How clients attach, and how many may.
3. How two hubs on one home are prevented, without stale-pid cleanup.
4. Which trust rules hold before any join flow exists.
5. Which builds carry it.

## Decision

- **A crate and a feature.** `crates/construct-hub` holds the hub; the
  binary's `hub` feature (in `full`, not `lean` or bare) adds `construct hub`.
  The crate links `construct-peer` with only its `accept` side. `construct
  manifest` lists the binary's features, so a build can be checked for the
  hub without reading symbols.
- **Everything in `$CONSTRUCT_HOME`, never `/tmp`.**
  `hub.key` (32 raw private bytes; public half derived), `hub.lock`,
  `hub.sock`, `hub/peers.json`. The home's `0700` (ADR 0021) is the outer
  wall. `hub.key` is created `O_EXCL` with mode `0600` and a key file group
  or others can read is refused (as ssh refuses one). `construct hub key`
  creates it on first use, so a peer can be stamped before a hub ever ran.
  Losing the key orphans every peer; back it up.
- **Control socket.** Newline-delimited JSON (`{"op":"status"}`,
  `{"op":"stop"}`) on `hub.sock`, chmodded `0600` straight after bind (a
  narrowed umask would be process-wide). Any number of clients attach at
  once. A home deep enough that the path exceeds 103 bytes (macOS
  `sun_path`) is refused with an error naming the fix.
- **One hub per home** by an exclusive `flock`-style lock (`File::try_lock`)
  on `hub.lock`, taken before the key, the port or the socket are touched. The
  kernel drops it when the process dies, crash included; the pid written into
  the file only names the holder in the refusal.
- **The hub is the registry's only writer.** With the lock as proof, `construct
  hub status` with no hub reads `hub/peers.json` and the key directly and
  reports the same shape (`running: false`, every peer offline), so status
  never depends on a TUI or a running hub.
- **Registry rows:** id, name, state (`expected` / `pending` / `pinned` /
  `revoked`), key (pinned, or offered while pending), last seen, and the ACP
  session and turn last reported (filled by #362; no journal `seq` cursor).
- **Admission, before #361:** a pure function of the registry, the key Noise
  IK authenticated and the id from the encrypted `Hello`:
  - a revoked key gets no message 2, and is refused under any id;
  - a pinned key gets the 16 MiB envelope cap; every other key keeps 4 KiB;
  - a known id (pinned or pending) arriving with another key is closed with
    a reason and its row is untouched;
  - an unknown id, or an `expected` slot, becomes `pending` with its key,
    capped at 8 pending rows (the oldest disconnected one is evicted; with
    all connected, the newcomer is refused);
  - nothing promotes a row to `pinned`: that is the join flow (#361);
  - ids are 1 to 64 of `[A-Za-z0-9._-]`, starting alphanumeric.
  A connection that fails the WebSocket upgrade, Noise IK or `Hello` within
  15 s leaves no row, no log line and no status entry.
- **Listening.** `construct hub --listen host:port` (or
  `CONSTRUCT_HUB_LISTEN`), default `127.0.0.1:9443`, plain `ws://`: Noise
  already authenticates and encrypts; TLS fronting (`tailscale serve`, a
  tunnel) is the lane's job (#364).
- **Shutdown** (`construct hub stop`, SIGINT, SIGTERM) sends every link a
  `Close`, waits up to 2 s, saves the registry and removes the socket.

## Consequences

- #361 adds approve/deny/revoke as new control-socket ops and registry
  transitions; the wire, files and lock stay as they are.
- A hub on a shared machine is protected by file modes alone; a root user or
  the same account can read the key. That is the same trust as the journal.
- Plain `ws://` on a non-loopback address exposes only the Noise responder:
  a dialer without the hub's public key fails message 1 silently.
- A second TUI or CLI never needs the lock; only `construct hub` itself does.
- Revisit the JSON registry if peers number in the hundreds (it is rewritten
  whole on every connect and disconnect).

## Evidence

- e2e `crates/construct-dev/tests/hub.rs`: a hub starts in a fresh home; its
  socket is `$CONSTRUCT_HOME/hub.sock`, a socket, mode `0600`; `hub.key` is
  `0600` and `hub key` prints the key the hub answers with; a second hub on
  the home refuses, naming the first's pid; an unknown peer completes Noise
  IK + `Hello` over `ws://` and appears `pending` and online in `hub status`;
  a pre-pinned peer appears `pinned` and online; a third connection claiming
  the pinned id with another key gets `Close` ("another key") and the pinned
  key is untouched; `hub stop` exits 0 and removes the socket; `hub status`
  then answers from disk; a restarted hub keeps the key. A second test runs
  `hub status`, `hub key` and `hub stop` with no hub.
- Unit tests in `construct-hub` (12): every admission rule above, the pending
  cap, the registry round trip, key creation mode and stability, the
  loose-key refusal, and the lock refusing a second holder.
- Release binary size (`[profile.release]`, `arm64-apple-darwin`), this
  branch on `origin/main` at `e5282b9`:

| build | bytes | hub linked |
|---|---|---|
| `full` without `hub` (`--features keyring,web,memory,skills,history,jobs,deck`) | 8,410,592 | no |
| `full` | 8,577,216 | yes, **+166,624 B (+1.98%)** |
| `lean` | 6,339,632 | no (`construct hub`: unrecognized subcommand) |
| bare (`--no-default-features`) | 5,792,752 | no (same bytes as ADR 0068's bare) |

  `construct manifest` on each lists `features` (`full` includes `hub`;
  `lean` and bare do not), and neither `lean` nor bare contains the hub's
  strings (`hub.sock`, "a hub is already running").
