# 0061 — The sandbox helper puts a memory ceiling on each tool's process tree

Status: accepted, 2026-11-21. Extends 0006 (the helper supervises each shell
call) and 0028 (the macOS helper); applies to MCP servers through 0010.

## Context

The sandbox bounded what a tool could read, write and reach, but not how much
memory it used (#230). In a Linux bench run a model's Python command
materialised an infinite iterator and grew to 111 GB in about 80 s; its own
`timeout 30` did not stop it. `systemd-oomd` then killed the whole cgroup it
lived in: the desktop app that launched the bench, 1,543 processes. A
terminal running the TUI would have gone the same way. The same model did it
three times, so it is a habit, not a one-off.

The issue listed the options and constraints:

1. **A cgroup v2 child (`memory.max`, read back from `memory.events`).** The
   most precise: counts everything, the kernel kills, the cause is definite.
   But it needs systemd delegation or a writable subtree, which Docker, CI
   runners and non-systemd distros often lack, and macOS has no cgroups at
   all. 0006 turned it down for process lifetime for the same reason.
2. **`RLIMIT_AS` / `RLIMIT_DATA`.** No privilege, but per process
   (`make -j32` passes 32 times), `RLIMIT_AS` counts address space (Go, the
   JVM, Node and anything mapping a large file fail far below their real use),
   and each program fails its own way (`MemoryError`, abort, segfault), so the
   harness cannot say why. macOS does not enforce `RLIMIT_DATA` or
   `RLIMIT_RSS` at all.
3. **A watchdog in the helper.** The helper (0006, 0028) already owns the
   call's whole tree: the subreaper's descendants on Linux, the process group
   on macOS. It can sum the tree's real memory on a timer and kill the tree
   past a ceiling. No privilege, no systemd, the same on both platforms and in
   CI, and since construct did the kill it can tell the model why.

## Decision

Option 3. `Policy` carries `memory_max: Option<u64>`, resolved from
`[sandbox] memory_max` when the policy is built. The helper's wait wakes every
`memory::SAMPLE` (100 ms) while it is set, sums the tree, and past the ceiling
writes `construct-memory <used> <limit> <prose>` to its stderr and sweeps the
tree as it does at the end of a call.

- **What a sample counts.** Linux: `RssAnon + VmSwap` of every descendant,
  walked through `/proc/<pid>/task/*/children` (a `/proc` scan by parent where
  that file is missing). File-backed pages are left out, so a local inference
  engine's mmapped weights, the page cache and shared libraries never count,
  and swap is counted so a cap does not just move the thrash to disk. macOS:
  `ri_phys_footprint` from `proc_pid_rusage` for every member of the group,
  the figure Activity Monitor shows as Memory. It includes compressed pages,
  which matters because macOS compresses a runaway before it swaps.
- **The setting.** `[sandbox] memory_max`: a share of RAM (`"50%"`, the
  default), a size (`"8G"`, `"512M"`, bytes) or `"off"`; one-run override
  `CONSTRUCT_SANDBOX_MEMORY_MAX`. A share, not a number, because one fixed size
  is wrong on either an 8 GB laptop or a 128 GB workstation. A value that does
  not parse fails config load. It applies with every `mode`, `off` included,
  like the rest of the helper's supervision. `construct manifest` (and every
  session's head event) shows `memory_max` and the `memory_max_bytes` it
  resolves to.
- **What the model reads.** The shell tool, when the report has the memory
  line, fails the call with code `memory` and says how much the command used,
  the limit and its setting's name, that it kept allocating, not to rerun it
  as is, and what to try instead. The kill signal is not reported as the
  cause. A background job puts the same note in its completion. An MCP server
  killed this way exits; the line's prose is in the server's stderr tail that
  the connection error already carries.

## Consequences

- A runaway tool dies on its own at half the machine's RAM by default, long
  before the 111 GB of #230, and the session continues. Measured on macOS:
  `list(itertools.count())` under a 1 GiB ceiling was killed at 1.2 GiB,
  0.33 s after it started.
- The ceiling is sampled, not enforced by the kernel: a tree can pass it by
  what it allocates in one interval (about 1.5 GB/s in #230, so a few hundred
  MB). Keep the ceiling well below RAM; 50% leaves room.
- **Not covered.**
  - The ceiling is per tree: per shell call, per background job, per MCP
    server. Several at once can together exceed it; there is no session-wide
    total.
  - On macOS a process that leaves the group (`setsid`) is neither swept nor
    counted (#9).
  - tmpfs files and POSIX shared memory (`RssShmem`) are not counted on Linux.
    The private `TMPDIR` is under `$CONSTRUCT_HOME`, on disk, so it does not
    matter there; `/dev/shm` is not writable from the sandbox.
  - It does not stop memory *pressure* caused by everything else on the
    machine: if other programs already hold most of RAM, a tool under its
    ceiling can still push the system into reclaim and oomd still aims at the
    whole app cgroup. A share of *available* memory, or a cgroup where
    delegation exists, would close that.
  - construct's own memory (the agent, the TUI) is outside the ceiling.
- **Revisit** if a sampled overshoot ever matters (then a cgroup where
  delegation exists, with this watchdog as the fallback, the "off, and say so"
  degrade the issue asked for), or if legitimate heavy work is killed at the
  default (raise the share, or make it relative to available memory).

## Evidence

- Issue #230 and its comments (the 111 GB and 24 GB runs, the model that
  wrote them).
- `crates/construct-dev/tests/sandbox_memory.rs`: a scripted shell call grows
  a string past a 16 MiB ceiling; the tool result names the limit and its
  setting, `tool_end` has code `memory`, the next call runs, construct exits
  0. Also: the manifest shows the ceiling, `off` removes it, a bad value
  fails. Passes on macOS and on Linux (Docker, `rust:1`).
- Manual, macOS: the same Python runaway as #230 under a 1 GiB ceiling,
  killed at 1.2 GiB after 0.33 s, exit 137.
