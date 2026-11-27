# 0077 — A peer stays up on macOS too (launchd), and a system service runs as a named user

Status: accepted. 2026-11-26. Part of #389. Builds on ADR 0021, 0076.

## Context

ADR 0076 left two gaps under "Not done":

1. `--install` refused on macOS, so a peer dropped on an old Mac did not
   survive a reboot.
2. A system unit had no `User=`, so a peer installed system-wide could only
   run as root. That is more than a diagnostic peer needs, even with the
   sandbox.

The exit-code contract had to hold on both platforms: revoked, refused,
denied, or `--require-key` with no key exits 77 and is not restarted.
launchd cannot match one exit code. `KeepAlive`'s `SuccessfulExit` only
tells zero from non-zero, and the peer's ordinary ends include exit 0 (the
hub closed the link) that must restart. A `/bin/sh` wrapper that maps codes
would also have to forward signals to the peer and keep its pid.

## Decision

- **launchd on macOS.** `construct peer --install` writes
  `construct.peer.<id>.plist`:
  - a LaunchAgent in `~/Library/LaunchAgents`, loaded with `launchctl
    bootstrap gui/<uid>` (it runs while the user is logged in), or
  - with `--system`, a LaunchDaemon in `/Library/LaunchDaemons`, loaded into
    `system` (it starts at boot with no one logged in).

  The binary goes to the same places as on Linux (`~/.local/bin`, or
  `/usr/local/bin` with `--system`), never inside the peer's `cwd`.
  `ProgramArguments` is the same explicit `peer --require-key --url …
  --hub-key … --id … --cwd …`. `WorkingDirectory` is the `cwd`, and
  `EnvironmentVariables` holds `CONSTRUCT_HOME` and `HOME`. `RunAtLoad`,
  `ThrottleInterval` 5 (systemd's `RestartSec=5`) and `ExitTimeOut` 30 are
  set. stdout and stderr go to `$CONSTRUCT_HOME/peer.log`, because launchd
  has no journal. A reinstall boots out the loaded job first and `enable`s
  the label. If a LaunchAgent cannot be bootstrapped (no login session, as
  over ssh), the plist stays and `--install` says it starts at the next
  login, and how to get boot-time start (`--system --user $USER`).
  `--uninstall` runs `launchctl bootout` and removes the plist. The binary,
  key and home stay, as on Linux.
- **"Do not restart" is a file that launchd reads.** `KeepAlive` is
  `{PathState: {$CONSTRUCT_HOME/peer.stopped: false}}`: launchd restarts the
  job after any exit while that file is absent. Every exit 77 first writes
  `peer.stopped` (`0600`, holding the reason), on both platforms
  (`service::mark_stopped`). `--install` removes it (`clear_stopped`), and a
  manual rejoin does not, so a stopped service waits for `--install` to run
  again, as a failed systemd unit does. systemd still reads the exit code
  (`RestartPreventExitStatus=77`). The file just tells an operator why.
  `KeepAlive` implies a speculative launch at load, so after a reboot a
  stopped LaunchAgent or LaunchDaemon runs once more. That run exits 77 at
  once when its key is gone. A refused key costs one handshake.
- **`--env` on macOS** writes `peer.env` (`0600`) as `export NAME='value'`
  lines, single-quoted so the shell expands nothing. A name that is not a
  shell identifier, or a value with a newline, is refused. The job runs
  `/bin/sh -c '. "$0" && exec "$@"' <peer.env> <binary> …`, so the keys
  never sit in the plist and `exec` keeps the peer's pid and signals. On
  Linux it is still systemd's `EnvironmentFile=` format.
- **`--system --user <name>`** (clap requires both `--install` and
  `--system`):
  - The account is looked up with `getpwnam_r`. The systemd unit gets
    `User=` and `Group=` (the primary group), and systemd sets `HOME`. The
    LaunchDaemon gets `UserName`, `GroupName` and `HOME`.
  - The peer's home is that account's own `~/.construct`, where it would
    have joined, unless `CONSTRUCT_HOME` names another. Under `sudo`,
    `HOME` is usually root's, and root's home holds no key.
  - Before anything is written, `--install` refuses an installer that is
    neither root nor that account. It also refuses a home whose parent
    directories the account cannot search, such as a home under `/root`.
  - After the env file is written and before the service starts,
    `account::hand_over` `lchown`s the home and everything in it to the
    account (symlinks themselves, never their targets). It sets the home to
    `0700` and `peer.key` and `peer.env` to `0600` (ADR 0021). It refuses,
    changing nothing, if any file there belongs to anyone other than root
    or that account.

## Consequences

- A Mac is now a drop target like a Linux box: join once with the token,
  then `--install`. The same no-restart contract holds, so an installed Mac
  peer that is revoked stops for good.
- A headless Mac reached over ssh has no GUI session for a LaunchAgent. It
  needs `sudo … --install --system --user $USER` to start at boot. This is
  the macOS counterpart of lingering.
- A system service no longer has to run as root. The binary stays in
  root-owned `/usr/local/bin`, out of reach of the account it runs as.
- `peer.stopped` is a new file in a peer's home. The peer never reads it.
  It is written on every exit 77, including a manual run's, so a peer that
  stopped for good leaves its reason on disk.
- launchd's `PathState` relies on file-system monitoring, which its man page
  calls race-prone for *starting* jobs. Here launchd checks it when the job
  exits, after the peer has already written the file, so no event can be
  lost.
- Not done: the workspace (`--cwd`) is not handed to `--user`. If that
  account cannot write there, the peer's tools fail and say so.

## Evidence

- Unit tests: `construct_peer::launchd` (plist keys, XML escaping, no `--`
  in comments, the env-file wrapper, labels, layouts, domains),
  `construct_peer::service` (`User=`/`Group=` replace root's `HOME`; stop
  marker written `0600` and cleared), `construct_peer::account` (lookup,
  hand-over tightens modes, closed parents named).
- e2e `crates/construct-dev/tests/peer_install.rs`: the real binary with
  stand-in `systemctl`/`loginctl`/`launchctl` on `PATH`, on whichever
  platform runs it:
  - It refuses an unjoined peer, `--user` without `--system`, and an
    unknown `--user`.
  - `--install` clears a previous `peer.stopped` and copies the binary
    `0755`. It writes the unit or plist with the restart rule and a `0600`
    env file that the unit or plist references and never inlines. It makes
    the right manager calls (`bootstrap gui/<uid>`, or `daemon-reload` and
    `enable --now`).
  - `--uninstall` removes the file and runs `bootout` or `disable --now`,
    and the binary and key stay.
  - It ran on macOS locally. Linux CI runs the systemd branch.
- e2e `stamp.rs`: a keyless `--require-key` exit 77 leaves `peer.stopped`
  with its reason.
- Live LaunchAgent on a real Mac (macOS 27, Apple silicon, debug build), a
  real `construct hub` on loopback, peer `live389`:
  - Joined once with the token, then `--install`. launchd started the job
    (`runs = 1`), and it rejoined with no token. The hub showed it `pinned`
    and online.
  - `kill -9` on the peer: launchd started it again at once (`runs = 2`,
    `last terminating signal = Killed: 9`), and it rejoined.
  - `hub revoke live389`: the peer deleted `peer.key`, wrote
    `peer.stopped` with the reason, and exited 77. launchd showed `last
    exit code = 77: EX_NOPERM`, `state = not running`, and `runs` stayed 2
    for 15 s.
  - Load as at boot (`bootout`, then `bootstrap` with `peer.stopped`
    present): one speculative run, which exited 77 at once ("needs a new
    join"). It was not restarted.
  - `--uninstall` booted out the job and removed the plist.
- Still to record: a reboot with the agent installed, and a LaunchDaemon
  with `--system --user` (both need a person at the machine and `sudo`).
  Linux `--user` is covered only by unit tests and CI.
