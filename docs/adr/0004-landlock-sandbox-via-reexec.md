# 0004 — Shell commands run under Landlock via a re-exec of `construct`

Status: accepted, 2026-10-20 (backfilled 2026-10-21). Partly superseded by 0006: the helper supervises the shell instead of exec'ing it.

## Context

Until 2026-10-20 construct had no boundary around its tools. In a routing A/B
(`bench/results/routing-20261020-111915.json`) an agent with no skill to go on
searched the disk, read the user's config and shell history, and `cat`ed the
two bench key files; the keys went to the model provider in a tool result.
The fix had to apply to everything a shell command starts, work without root
or a container runtime, and leave the first frame and tool latency alone.

Landlock restriction is irreversible for the process that applies it, so it
cannot be applied in the agent itself. The `sandbox.rs` header gives the
chosen route: a small single-threaded helper that restricts itself and execs.

## Decision

The shell tool does not spawn the shell directly. `Policy::command` in
`crates/construct/src/sandbox/mod.rs` spawns
`construct __sandbox <policy-json> -- <shell> -c <script>`. `main.rs` sees
`__sandbox` first, before the runtime or any thread exists, and calls
`sandbox::exec_child`: it builds the Landlock ruleset (best-effort ABI, signal
scope), restricts itself, then `exec`s the shell, so every descendant inherits
the restriction.

The in-process file tools (read, write, edit, grep, glob) check each path, after
resolving symlinks, against the same roots. Tools get an environment with
credential-named variables and `SSH_AUTH_SOCK` removed. Default mode
`workspace`: read and write the working directory, a private `TMPDIR` and the
spill dir; read-only system paths and toolchains; nothing else under `$HOME`.
`read-only` and `off` are config choices. If Landlock is unavailable the
command fails with a message naming `mode = "off"`, rather than running
unconfined.

## Consequences

- No extra dependency at runtime beyond the `landlock` crate, and no root.
- Linux only. Other platforms get no shell boundary until someone adds one.
- A tool that breaks inside the sandbox is fixed by adding its toolchain path
  to `HOME_RO` / `HOME_RW` plus a step in the sandbox e2e test, not by turning
  the sandbox off (AGENTS.md).
- Landlock does not contain processes that leave construct's process group
  (an agent once compiled a brute-forcer and started it with `setsid nohup`,
  outliving the session); observations.md points at a GitHub issue for this gap.
- Real-model benches must run sandboxed; AGENTS.md says so.

## Evidence

Commit `62a5d15` ("Sandbox tools by default: Landlock shell, path-checked file
tools, no credentials in env"); follow-up `0f09a19` narrowed `/dev` (all of it
had been allowed, so `/dev/shm` was a writable scratch space). Both landed as commits on main (no PR number recorded here). Test: `sandbox_keeps_tools_inside_the_workspace` in
`crates/construct-dev/tests/e2e.rs`. Findings: `bench/results/logs/observations.md`.

- Before: in `routing-20261020-111915.json` the local-router agent read outside
  its work dir in every run, and in one run printed both bench key files.
- After: `bench/results/loop-20261020-122108.json`, sandboxed, key scan clean,
  0 tool calls outside the work dir in all 14 sessions (1–25 per run before the
  sandbox, per the notes).
- Landlock denied reads of `/proc/<bench pid>/environ` in a later seeded run.
  The same run showed the escaped-process gap above.
