# Search engines, measured (issue #10)

> Decision: docs/adr/0021-search-tool-stays-grep.md. fff is not adopted; this
> is the record of why, and of how the engines were compared.

A standalone crate (its own `[workspace]`, not built by `cargo xtask check`)
that compares the engines a `grep` tool could sit on, with no model involved.
It exists to answer "is fff faster, and is it right?" before any of it is
wired into construct.

```sh
cd bench/search-engines && cargo build --release
target/release/search-engines <corpus-dir> [reps]       # speed, then correctness vs the rg binary
target/release/fresh <corpus-dir> [watch|nowatch] [idx]  # does the index see files written a moment ago
```

Engines: `seq` is a copy of construct's grep today (one thread walks, then
searches). `par` is the same ripgrep crates (`ignore`, `grep-searcher`) on
every core. `rg(bin)` is the ripgrep binary, spawn included. `fff` is
`fff-search` 0.11.0 (`FilePicker` + `grep`), with the file watcher off, on, or
the content (bigram) index on. The query is built as a `FFFQuery` directly,
never parsed from a string (see "Findings", fidelity).

Corpora: shallow clones of ripgrep (220 files), tokio (871), cargo (2.8k) and
the Linux kernel (97k). 24 cores, warm page cache, median of N runs. Raw
output: `results/`.

## Findings

**Speed (linux, 97k files, ms, median of 5).** fff and `fff+idx` stop at 200
matches, as the tool's output cap does; `seq`, `par` and `rg` count every
match. So only the no-match and rare-literal rows compare like for like.

| query | seq (today) | par | rg bin | fff | fff + content index |
|---|---|---|---|---|---|
| rare literal (537 hits) | 528 | 71 | 74 | 21 | 3.2 |
| no match (full scan) | 525 | 71 | 72 | 44 | 10.5 |
| regex | 1387 | 150 | 86 | 3.4 (capped) | 1.7 (capped) |
| ignore-case | 617 | 83 | 91 | 1.0 (capped) | 0.8 (capped) |

On the 3k-file cargo repo every engine but `seq` answers in under 10 ms (par
5 ms, fff 0.3 ms). A model request takes seconds, so at the sizes agents
usually work in, search latency is not what a session waits on.

- **Most of the gain over today's grep is parallelism, not fff.** `par` is the
  same crates on every core: 4x on cargo, ~9x on linux, no new dependency. Any
  fair claim for fff has to beat `par`, not `seq`. fff beats it by 3x (no
  index) to 7x (index) on a full scan of linux, and by ~20x on a rare literal
  with the index, at 480 ms of background indexing and ~350 MB RSS.
- **Fidelity (fff 0.11.0, regex mode): wrong line numbers.** `^\s*impl\b`
  reports the blank line *before* an `impl` (the whole buffer is searched, so
  `\s` crosses `\n`). Extra lines against rg: +425/433 (ripgrep), +1847
  (tokio), +939 (cargo), +484 (linux). Fixed in `0.11.1-nightly.d1c7dd4`,
  not in a stable release. Plain-text and case-insensitive searches matched rg
  exactly on every corpus except for the policy differences below.
- **Policy differences from today's tool.** fff searches hidden files
  (`.cargo/config.toml`) which the tool skips, and skips files with NUL bytes,
  which rg searches up to the NUL.
- **Query-string parsing is a hazard.** fff's grep takes one string with
  constraints inside it (`*.rs`, `!test/`, `src/`, and with `AiGrepConfig` a
  token like `self.tx` becomes a file-path constraint). Passing a model's regex
  through that parser changes what it asked for, so a wrapper must build
  `FFFQuery` itself.
- **Freshness (`fresh`).** With the watcher off the index never sees a new or
  edited file (5 s timeout, linux and cargo). With it on, a file written by
  the process is visible after ~70-80 ms; a grep issued right after a write
  misses it. An agent edits constantly, so the watcher is mandatory and a
  tool call that follows a write needs a barrier.
- **Cost.** `fff-search` is 116 crates with vendored libgit2 and LMDB; a cold
  release build of a crate that only names it takes ~75 s. 14-17 threads. RSS:
  +27 MB on cargo, +165 MB on linux, +350 MB on linux with the content index.

## Not measured here

Tool-call behaviour with real models. That was measured afterwards, on the
ripgrep engine, and is in the ADR above and `bench/results/search-report-*.md`.
Whether fff's ranking or typo tolerance changes how many steps an agent takes
was not tested: fff was dropped before that arm was built.
