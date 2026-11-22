# 0034 — The journal survives a page-cache loss

Status: accepted. 2026-11-02. Implements #161. Amends ADR 0008's
`synchronous=NORMAL` and its "no fsync per row" consequence.

## Context

ADR 0008 made the journal the conversation's record and promised a row is
written before its event is sent. It opened SQLite in WAL mode with
`synchronous=NORMAL`, which only guarantees that a crash of the construct
process cannot corrupt the database: the last commits may still sit in the
operating system's page cache.

Deck runs construct in a libkrun guest whose journal disk honours flushes.
Killing the VMM after a completed ACP turn left an empty journal image; the
same turn with a guest-wide `sync` left rows and a WAL. A VMM death discards
the guest page cache, so a commit the client already saw is lost even though
the process was idle and healthy (strf0x1/deck#16, `docs/vm-spike.md`).

`synchronous=FULL` alone did not fix the VM case. Re-running the turn against
the FULL build still left no `projects/` directory, no `journal.sqlite` and no
WAL after `kill -9`, while a guest-wide `sync` made them appear. A control
file written with `dd conv=fsync` also vanished: the missing layer is the
**directory entry**. SQLite fsyncs the files it writes, never the directories
that name them, so the database and its WAL can be durable and unfindable at
the same time.

The alternatives were a checkpoint at turn boundaries and an explicit flush
request/ack. Checkpointing bounds the loss to the current turn and does not
cover a crash mid-turn, while every event is sent as it is journaled; a flush
protocol adds a handshake to the ACP extension and still has to be implemented
by callers. Both also leave the promise dependent on a graceful stop, which is
exactly what a VMM death or `kill -9` denies.

## Decision

The journal opens with `journal_mode=wal` and `synchronous=FULL`
(`journal/mod.rs`). Each `Journal::append` is an autocommit insert, so SQLite
fsyncs the WAL at commit and `append` does not return until the row is on
disk. `Writer::emit` sends the event only after that append, preserving ADR
0008's ordering with a real durability boundary behind it.

Directories are synced too:

- `Journal::open` creates a missing project path with `create_dir_all` and
  then fsyncs the parent of every directory it created, so `projects/`,
  `<project>/` (and `$CONSTRUCT_HOME` if it was missing) are linked before
  files are written below them.
- The first commit in a process may create `-wal`/`-shm`; `append` fsyncs the
  journal's directory once, after that commit, so those entries are durable
  before the row is sent. Later commits reuse the existing files, so this is
  not a per-event cost.

Batching is confined to events that are sent together (see the amendment
below): the whole group is committed before any of its events is sent, so the
row-before-send promise still holds. Events that become durable at different
times keep their own fsync. The measured cost before the amendment was one
fsync per durable event (eight per scripted turn).

Readers use the same `Journal::open`, so a read-only session also sets FULL on
its own connection; it writes nothing, so this costs nothing.

## Amendment (2026-11-21, #248): group commit for events sent together

Parallel tool calls finish as a group: their `tool_result` rows are all ready
after `join_all`, and they are sent together. They used to be N autocommit
inserts, each with its own WAL fsync. `Writer::emit_batch` now commits such a
group in one SQLite transaction and sends none of its events until that
transaction returns. `Journal::append_batch` is the only new journal
operation; every event that is not part of a ready-together group still uses
`append`.

Invariants kept:

- one seq per event, assigned in emission order;
- no event sent before its row is durable (ADR 0008);
- the group's rows are committed under the writer lock and its events are sent
  in seq order from the same lock;
- a crash after any row still restores without re-running a tool (ADR 0029);
  the #250 sweep covers every cut.

Scope is deliberately narrow. `tool_start` is committed when a call starts
and `tool_end` when it finishes, so those stay individual commits; only the
`tool_result` rows pushed after all calls finish are grouped. Holding rows
until a turn boundary, as considered in the original decision, stays rejected:
it would delay unrelated events behind the slowest one.

Measurements (`bench/group_commit.py`, scripted model, 3 turns × 5 reps) before
the amendment:

| N parallel calls | rows = fsyncs/turn | local p50 | guest p50 | WAL FULL commit, guest |
|---|---|---|---|---|
| 1 | 14 | 5.73 ms | 180.98 ms | 11.61 ms |
| 4 | 23 | 8.44 ms | 298.19 ms | 11.54 ms |
| 16 | 59 | 20.64 ms | 783.62 ms | 11.33 ms |

N of those rows per turn are `tool_result`. Grouping them removes N−1 guest
fsyncs: ~35 ms (12 %) at N=4 and ~170 ms (22 %) at N=16. The local saving is
~0.2 ms and ~1.2 ms. The guest number is the one that decides, because a
guest disk forwards flushes and local APFS does not cost the same.

After the amendment (same command; WAL commit frames counted, so grouped rows
are one fsync; 15 samples per cell):

| N | guest commits/turn before → after | guest p50 before → after |
|---|---|---|
| 1 | 14 → 14 | 181 → 182 ms |
| 4 | 23 → 20 | 298 → 262 ms |
| 16 | 59 → 44 | 784 → 592 ms |

`bench.py compare` (tier 1) calls `n16.turn_ms` and `n4`/`n16` commit counts
`better`, `n1.turn_ms` and `n4.turn_ms` `same` under its spread rule, and
nothing `worse`. The N=1 turn has no group to commit, as expected.

## Consequences

- A completed turn survives an abrupt process, VM or host death with every
  event the client acknowledged. A `session/close` is no longer load-bearing
  for durability.
- Directory fsyncs are paid once per process open (only when a directory was
  created) and once after the first commit; they do not add to the per-event
  cost.
- Every durable event costs one fsync instead of none. On APFS with a warm
  file system the measured cost is ~1.6 ms per scripted turn (~0.2 ms per
  row); a guest disk that forwards flushes will cost more, bounded by its
  flush latency. Byte-for-byte amplification is unchanged: the same rows and
  pages are written, only the number of flushes rises.
- WAL checkpoints still happen automatically (1000 pages); the WAL file is
  read back normally after a crash.
- A process that emits many durable events per turn pays one fsync each,
  except the `tool_result` rows of one turn, which share a single transaction
  (amendment above). Other events still become durable as they are emitted.
- Revisit if: the journal moves to storage with slow `fsync` (network or
  emulated disks) and turn latency is dominated by it, or the event rate makes
  per-row flushes unreasonable for rows that are not sent together.

## Evidence

- #161 (finding, options, acceptance); strf0x1/deck#29 and its
  `docs/vm-spike.md` for the VMM-death measurement.
- Unit test `journal::tests::wal_commits_are_fsynced_before_append_returns`
  asserts WAL + `synchronous=2` and reads the row back after reopening.
- Unit test `journal::tests::a_new_project_directory_and_its_wal_are_linked`
  opens a journal in a missing project path, appends, and asserts the
  directories, database and WAL exist, then exercises `sync_dir` on the
  directory.
- e2e `acp::a_completed_turn_is_on_disk_after_a_kill_without_close` drives
  initialize, `session/new` and a scripted prompt, SIGKILLs the agent without
  `session/close`, and reads the final message and `turn_end` back from the
  journal. It proves the journal is complete after a process crash, not after
  a page-cache loss: SIGKILL leaves the host page cache intact, so it would
  pass with NORMAL as well. The VM proof is Deck's re-run below.
- Turn cost, `construct acp` against the scripted endpoint, 40 prompts per
  run, three runs each, warm APFS (`bench`-style throwaway harness, debug
  build):
  median 106.12 ms/turn with NORMAL vs 107.75 ms/turn with FULL (+1.6 ms,
  +1.5%). Journal after 40 turns: 114,688 B in both arms, `-wal` checkpointed
  to 0 B on exit; 323 rows, 8.1 durable rows per turn.
- Deck re-run against the FULL-only build (strf0x1/deck#29): completed ACP
  turn + `kill -9` still lost the journal; with a guest-wide `sync` it held
  `journal.sqlite`, a 243 KiB WAL and 11 rows. Re-running that check against
  the directory-synced build is the acceptance proof for this ADR; it is not a
  local test because no in-process test can drop the page cache.
