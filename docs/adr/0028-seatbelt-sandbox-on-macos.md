# 0028 — Shell commands run under a Seatbelt profile on macOS

Status: accepted, 2026-10-31. Extends 0004 (Landlock); changes what 0017 can
promise about a delegated child's own process.

## Context

0004 made the shell boundary Landlock, which is Linux only. On macOS the
`landlock` crate does not compile, so there was no macOS build at all, and the
supervisor from 0006 used `prctl` and `/proc`. #37 asked for the same boundary
on macOS from the same `Policy`.

Options for the filesystem boundary:

1. **Wrap the shell in `sandbox-exec -p <profile>`.** Codex and Anthropic's
   sandbox-runtime do. The tool is marked deprecated but ships.
2. **Call `sandbox_init_with_parameters` from the helper.** The libSystem call
   `sandbox-exec` itself makes, and what Chromium and Firefox use. It is not in
   the SDK headers, so it is declared by hand.
3. **A container or VM.** Out of scope: no root, no runtime, no startup cost
   was the premise of 0004.

## Decision

Option 2. `construct __sandbox` builds an SBPL profile from the same `Policy`
(`sandbox/seatbelt.rs`), applies it to itself, then forks and supervises the
shell exactly as on Linux, so everything the command starts inherits it. The
profile is `(deny default)` and grants:

- read and `file-map-executable` on `system`, `read` and `write` roots (Seatbelt
  keeps mapping code apart from reading; Landlock's read includes execute);
- write on `write` roots, `file-read-metadata` on the directories leading to a
  root and on `/etc`, `/var`, `/tmp` (symlinks into `/private`), and nothing
  else under `$HOME`;
- network: all, or with `no_network`, outbound to `localhost:<port>` for each
  `connect_ports`;
- signals only to processes in the same sandbox;
- what a process cannot start without: process exec/fork, `mach-lookup`,
  `sysctl-read`, `system-info`, `/dev/dtracehelper`, and the `xcrun_db` cache
  file in the per-user temp dir (git and cc go through `xcrun`, which ignores
  `TMPDIR`).

The platform split is `landlock.rs` / `seatbelt.rs` for `restrict`, and
`supervise.rs` / `supervise_macos.rs` for the supervisor. `SYSTEM` and `HOME_RW`
differ per OS; the credential-free environment (`scrub_env`) is shared.

The macOS supervisor has no subreaper and no parent-death signal. It runs the
shell in its own process group and waits on one kqueue for: the shell exiting,
construct exiting, and SIGTERM/INT/HUP. On any of them it SIGKILLs the group and
reports its members as `construct-swept` lines, as on Linux.

File tools open through `sandbox/beneath.rs`: `openat2` on Linux, and on macOS
`openat` with `O_NOFOLLOW_ANY`, which refuses a symlink anywhere in the path
(and `O_EVTONLY` where Linux uses `O_PATH`).

## Consequences

Easy: the same `Policy`, config, permission prompts and tests on both systems.
A denial reads `Operation not permitted` (EPERM) where Landlock says `Permission
denied`; the prompt heuristics already matched both.

Hard, and accepted:

- **Seatbelt cannot nest.** A sandboxed process cannot apply another profile
  (`sandbox_init` returns EPERM; Landlock stacks, Seatbelt does not). 0017 starts
  a delegated child under a network-and-signals-only lock and then lets it lock
  each shell call. On macOS the child's own process gets no profile, and each
  of its shell calls gets the full one. What is lost: a child with `network =
  false` that has the `web` tool group would reach the network from its own
  process; its shell is still confined. Fix when it matters: refuse the `web`
  group for a child without network on macOS.
- **A process that leaves the group is not swept** (`setsid`, `daemon`). On
  Linux the helper is a subreaper and sweeps it; macOS has no equivalent. This
  is #9, and the lifetime e2e tests start their escapees with `setsid` only on
  Linux.
- **Private API.** `sandbox_init_with_parameters` and `sandbox-exec` are both
  deprecated or undocumented. If Apple removes the call, the helper fails with
  a message naming `mode = "off"` rather than running unconfined.
- **Broad Mach and sysctl access.** `mach-lookup` is allowed in full; tools
  need DNS, the keychain, `xcrun`. Narrowing it is a later pass.
- **Metadata of the path leading to a root is visible**, as stat of `/`, `/Users`
  (if the workspace is under it) and so on. Contents and listings are not.

Not covered: macOS earlier than the one tested, and Intel hardware (CI builds it
on an Intel runner but this was measured on Apple silicon only).

## Evidence

#37. Tested on Apple silicon, Darwin 27.0.0, with the debug and the release
(LTO) binary:

- `sandbox_keeps_tools_inside_the_workspace` passes. Its `/proc` probe became
  `ps eww` on macOS, which is itself refused inside the sandbox.
- By hand, in the helper: a read of `~/.ssh`, `~/Documents` and
  `~/.zsh_history`, a read next to the workspace, and `kill -0` of a process
  outside it are all refused; a write inside the workspace and `curl
  https://example.com` work; `git`, `cc`, `cargo build`, `python3`, `node` and
  Homebrew tools run under the default policy.
- A `sleep 300 &` left by a shell call is killed when the call ends; killing
  construct mid-call stops the command tree.
- `cargo test --workspace`, clippy and fmt pass on macOS. Fixed along the way:
  tests that assumed `/proc`, GNU `sed -i`, GNU `base64 -w0`, GNU `ls` error
  text in two snapshots, and a temp dir under the `/var` symlink.
- `cargo xtask check`'s `bench` gate (Python tests) fails on macOS on `main` as
  well: its temp-dir cleanup races git's background gc. CI's macOS job runs
  fmt, clippy and the tests without it.
