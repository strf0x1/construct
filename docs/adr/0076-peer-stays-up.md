# 0076 — A peer stays up: a systemd unit, one no-restart exit code, a revoked key told so

Status: accepted. 2026-11-26. Part of #273, #367. Builds on ADR 0029, 0034,
0045, 0069, 0070, 0071, 0072, 0073; amends 0069 and 0070 on what a revoked
key gets.

## Context

A peer dropped on a headless box (ADR 0073) ran only as long as the shell
that started it. #367 asks that a joined peer stay joined across reboots and
crashes without anyone touching it. Constraints:

1. The peer's own tools must not be able to overwrite its stamped binary.
   They can write the peer's workspace (`cwd`) and nothing else under
   `$HOME` (ADR 0028; `~/.local/bin` is read-only in the sandbox).
2. `Restart=always` must not tight-loop a peer the hub will never take back.
   Before this change, a peer revoked while it was offline could not tell:
   the hub answered its key with silence (ADR 0069), the same as a hub that
   is down. It would redial forever.
3. A peer that crashed mid-turn must not run that turn again (ADR 0045), and
   the hub must see that the turn was cut off.
4. No secret in the unit file. With `llm = "hub"` (ADR 0072) a peer needs
   none. With `llm = "local"` (ADR 0051) it needs API keys after a reboot.
5. A hub that is down at boot, or a network that is not up yet, is normal
   for a box that just started.

## Decision

- **`construct peer --install`** (also `./construct-<name> --install` on a
  stamped binary: a stamped binary whose first argument is a `construct
  peer` flag runs `peer`):
  - Installs only a peer that has joined (`peer.key` exists), so the unit
    never carries a join token. `--join` conflicts with it.
  - Copies the running binary to `~/.local/bin` (`$XDG_BIN_HOME`), or to
    `/usr/local/bin` with `--system`, by temp file and rename, so a
    reinstall never writes into a running binary. It refuses a bin dir
    inside the peer's `cwd`, where the peer's tools could write.
  - Writes `construct-peer-<id>.service` to `~/.config/systemd/user`, or to
    `/etc/systemd/system` with `--system` (run as root, `HOME` set
    explicitly), then runs `daemon-reload` and `enable --now`. A user unit
    also gets `loginctl enable-linger`. Without lingering it starts at
    login, not at boot, and `--install` says so.
  - The unit: `ExecStart=<bin> peer --require-key --url … --hub-key … --id
    … --cwd …` (every value explicit, whether or not the binary is
    stamped), `WorkingDirectory=` the `cwd`, `Environment=CONSTRUCT_HOME=`
    the home holding the key, `Restart=always`, `RestartSec=5`,
    `StartLimitIntervalSec=0`, `RestartPreventExitStatus=77`,
    `After=`/`Wants=network-online.target`. Rendered and quoted for systemd
    (`%` and `$` escaped) in `construct_peer::service`.
  - `--env NAME` (repeatable) copies variables from the installing shell
    into `$CONSTRUCT_HOME/peer.env` (`0600`), referenced by
    `EnvironmentFile=`. An `llm = "local"` peer with no `--env` gets a
    warning.
  - `--uninstall` disables, stops and removes the unit. The binary, key and
    home stay.
  - systemd only. On macOS `--install` refuses (amended by ADR 0077:
    launchd).
- **Exit 77 means do not restart.** The peer exits with
  `service::EXIT_NO_RESTART` (77, sysexits' `EX_NOPERM`, shown as
  `status=77/NOPERM`) when the hub revoked, refused or denied it, and when
  `--require-key` finds no key. Every other end (a crash, a lost hub before
  it ever joined, a bad flag) keeps the old exit, and the unit restarts it.
  77 rather than 3: systemd names 3 `NOTIMPLEMENTED`.
- **`--require-key`** never creates a key. A missing `peer.key` means a new
  join: it prints what to run at the hub (`revoke` if still pinned, then
  `invite`) and exits 77. It also makes the peer redial from the start, as
  a peer that has joined before does after a lost link. A hub that is down
  at boot is retried with backoff, not an exit.
- **A revoked key is told so.** The hub completes Noise IK for a revoked key
  at the pre-pin envelope cap. After the 1-RTT `Hello`, admission answers
  it `Admission::Revoked`. The hub sends `Control::Revoke`, closes, and
  logs one line. Nothing else reaches that link: no row, no ACP, no model
  call. The peer deletes `peer.key` and exits 77. A new key on a revoked
  id is still refused with `Close` ("was revoked").
- **A transport that drops before a verdict is `Lost`, not `Refused`.** A
  hub that turns a peer away always says so with `Close`. Exit codes now
  depend on the difference.
- **Count a cancelled call's usage** (`construct-hub` `proxy.rs`, from
  #371). The engine hangs up once it reads `[DONE]`, often before the hub
  sees the upstream body end, and the gateway then sends `Cancel`. The hub
  used to abort the task and drop the usage it had already read. A cancel
  now stops the call upstream and still ends it with what it used.

## Consequences

- A dropped box is two commands: run it once with the token, then
  `--install`. After that, reboots, crashes and hub restarts need nothing.
- Crash recovery is what ADR 0029, 0034 and 0045 already give. On reattach
  (ADR 0071) the hub resends its unanswered prompt with its key. The peer
  answers `cancelled` and never re-runs it. Proven by e2e and on a real box.
- A revoked key that is still held learns it was revoked, which it would
  learn anyway the next time it was online. It costs the hub one handshake
  and one log line per attempt. The attempts are bounded: an installed peer
  that is told stops for good.
- A peer whose key was deleted by hand needs `hub revoke` and `invite` at
  the hub, as before. `--require-key` only makes that say so instead of
  waiting for an approval that cannot come.
- A peer waiting for approval redials when the hub restarts or the link
  drops (#388), as ADR 0070 expected. Once the hub has said `Pending`, a
  `Close` ends the link instead of refusing the peer. The redial is a new
  handshake, so both ends show a new code and the old one approves nothing.
  Admission runs again: a pairing window that closed with the old hub
  refuses the peer, and the pending cap and strike rules are unchanged.
- Not done here: launchd on macOS, and a `User=` for system units other
  than root. Both are ADR 0077 (#389).

## Evidence

- e2e `crates/construct-dev/tests/peer_restart.rs`, real `construct hub` and
  `construct peer` processes on loopback with a scripted model:
  - **SIGKILL mid-turn.** The restart (`--require-key`, no token) reloads
    the same key and rejoins with no approval. The hub's session shows the
    turn `cancelled` with no prompt left. The tool's effect happened once,
    there is one model request and one `submitted` row, and a `recovered`
    row exists.
  - **Revoked while down.** The restart exits 77 with "the hub revoked
    old-pc" and deletes `peer.key`. Run again, it exits 77 with "needs a
    new join" and creates no key.
  - **Hub down at boot.** The peer keeps dialing ("cannot reach the hub")
    and joins once the hub starts on the same port.
  - **Hub restarted while a peer waits (#388).** A clean `hub stop` and a
    SIGKILL each make the peer redial. It prints a new code, `hub pending`
    lists only that one, and `approve` pins the peer's key. A peer that
    walked up in a pairing window is refused after the restart and exits
    77.
- e2e `join.rs`, updated: a live revocation exits 77. The old key under a
  new id now completes the handshake, gets `Revoke` then a close, and
  leaves no row.
- e2e `peer_llm.rs`: before the proxy fix, the hub counted only one of a
  turn's two calls in 4 of 6 runs of the peer suites. That happened
  whenever the suite ran enough peers at once to delay the upstream EOF.
  After the fix, 5 of 5 runs passed.
- Unit tests: `construct_peer::service` (unit lines, quoting, layouts, env
  file, binary placement), `admit.rs`/`registry.rs` (a revoked key gets the
  pre-pin cap and `Admission::Revoked`).
- e2e `stamp.rs`: a stamped binary run with `--require-key` and no key
  exits 77 ("needs a new join") without making a key, and `--install`
  refuses a peer that has not joined. Both route to `peer`, while
  `--version` and `config` still do not.
- A real box (#367 PR): x86_64 Arch, systemd 262, a `bare`+`peer` binary
  stamped by `construct build --from`, the hub on a Mac over the LAN.
  Covered: join, `--install`, a prompt through the installed unit,
  `kill -9` mid-turn (systemd restarted it in 5 s, it rejoined without a
  token, the hub saw `cancelled`, the effect ran once, no stray process),
  revoked while stopped (the no-restart exit, key deleted, `NRestarts=0`;
  run while the code was still 3, before it became 77), `--uninstall`,
  and reinstall over a running binary. A reboot while installed: the
  lingering user manager started the unit in the same second userspace was
  up (`NRestarts=0`). sddm autologin also started then, so this boot does
  not separate the two. The peer retried through `Network is unreachable`,
  rejoined 17 s later without a token, and the next prompt ran in the same
  session with its model call through the hub.
