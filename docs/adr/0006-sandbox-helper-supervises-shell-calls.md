# 0006 — The sandbox helper supervises each shell call; nothing outlives it

Status: accepted, 2026-10-21. Partly supersedes 0004 (the helper no longer
execs the shell).

## Context

0004 restricts *what* a shell command can touch, not *how long it lives*. The
shell tool killed a timed-out or interrupted command by process group, and
`setsid nohup ./thing &` leaves the group. In a seeded loop run an agent
compiled a SHA-256 brute-forcer, started it that way, and it was still running
1.5 hours after the bench finished (#9, `bench/results/logs/observations.md`).
A second symptom: a background job that inherits stdout kept the pipe open, so
the call hung until its timeout.

Options weighed in #9 (research for #17): run the agent in a microVM or
container (a hypervisor or a daemon, which is not construct); a PID namespace
(off by default for unprivileged users on Ubuntu ≥ 24.04, so it needs a
fallback anyway); a cgroup v2 scope (needs systemd or a delegated subtree,
absent in many containers and CI); `PR_SET_CHILD_SUBREAPER` (no dependency, no
privilege, kernel ≥ 3.4). macOS has none of the Linux mechanisms and is a
separate PR.

## Decision

`construct __sandbox` (`crates/construct/src/sandbox/supervise.rs`) applies
Landlock to itself as before, then, instead of exec'ing the shell:

- sets `PR_SET_CHILD_SUBREAPER`, so orphans reparent to it, not to init;
- sets `PR_SET_PDEATHSIG` to SIGUSR1 and treats it as "construct died" only
  when `getppid()` is no longer construct's pid (the signal also fires when the
  spawning tokio thread retires);
- blocks SIGCHLD, SIGTERM, SIGINT, SIGHUP and SIGUSR1, spawns the shell, and
  waits with `sigwaitinfo`;
- when the shell exits, on SIGTERM, or when construct dies, sweeps: SIGKILL
  every direct child from a snapshot, reap, repeat until none are left or 2s
  pass; then exits with the shell's status (re-raising its signal).

Lifetime is per call: leftovers die at the end of each shell call, so
`server & curl …` in one call still works. Anything meant to outlive a call
belongs to background delegation (#34) or a scheduler (#11), not to `&`.

The helper writes `construct-swept <pid> <comm>` lines to its stderr; the shell
gets stdout as its stderr, so it never writes there. The shell tool appends
`[stopped N background process(es): names]` to the result. On timeout or
interrupt, and when the call's future is dropped, the tool sends SIGTERM to the
helper so it can sweep, and kills the process group only after a 3s grace.

The helper also runs with `[sandbox] mode = "off"`, skipping only Landlock:
process lifetime is hygiene, not part of the filesystem boundary.

The subreaper is in the per-call helper, never in construct itself, where it
would also catch the detached learner and MCP servers and contend with tokio's
child reaping.

## Consequences

- A command cannot leave anything running after its call, whether it used
  `&`, `nohup` or `setsid`. Background jobs holding stdout no longer stretch a
  call to its timeout.
- One extra process per call stays alive for the call's duration (it used to
  be replaced by the shell). Cost measured below.
- Not covered: something started through a socket outside the tree
  (`docker run -d` via the Docker socket, `systemd-run --user` via the session
  bus); Landlock as configured does not restrict connecting to pathname Unix
  sockets. A process stuck in uninterruptible sleep can outlast the 2s cap.
- The shell's `$PPID` is now the helper, which shares its Landlock domain, so
  `/proc/$PPID/environ` is readable; that environment is already scrubbed.
  construct's own environ (the grandparent) stays denied, which the sandbox e2e
  test now checks.
- macOS (no subreaper) and child constructs from #32/#34 should reuse this
  helper; both are follow-ups.

## Evidence

- e2e (`crates/construct-dev/tests/e2e.rs`): `shell_calls_leave_nothing_running`
  (and `_with_the_sandbox_off`) cover `setsid nohup sleep 300 … &`, `sleep 300 &`
  with stdout not redirected, and `setsid sleep 300 & sleep 30` with
  `timeout: 1`; `killing_construct_mid_call_stops_the_command_tree` SIGKILLs
  construct mid-call. Learning e2e and golden request snapshots unchanged.
- Tier 1 `bench.py tools` (mock model, 5 rounds × 15 calls), before
  `bench/results/tools-20261021-175219.json` (main 0131fe5), after
  `bench/results/tools-20261021-175425.json`; `bench.py compare`
  (`compare-20261021-175425.json`):

  | metric | before | after | verdict |
  |---|---|---|---|
  | read (ms) | 0.457 [0.338–0.652] | 0.400 [0.332–0.486] | same |
  | shell (ms) | 2.32 [1.96–2.68] | 2.36 [2.18–2.61] | same |
  | wall (s) | 0.0702 [0.0641–0.0816] | 0.0690 [0.0644–0.0747] | same |
