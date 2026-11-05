# Rust build caches for parallel agent worktrees

Research note, accessed **2026-11-04**. This is a survey and a proposed pilot,
not an adoption decision or a performance result. Nothing was installed and no
build configuration was changed. Source was inspected at these revisions:

- [Kache `97d7b802`](https://github.com/kunobi-ninja/kache/tree/97d7b802a5f870ba9abfc645bd34609f70791a23): source package 2.0.1; README still recommends 2.0.0.
- [mr-boxington `599e5b6e`](https://github.com/jdx/mr-boxington/tree/599e5b6e105d4006373ef2240c676026c6dab74f): mbx 1.29.2.
- [huacnlee/cargo `6ebb8919`](https://github.com/huacnlee/cargo/tree/6ebb89197f5b3a88b637e28672e6d39d67343401): a Cargo fork.

**[V]** below means verified in primary source/documentation, not proven in a
Construct build. **[I]** means an inference or recommendation. Vendor benchmarks
are not comparable head to head and are not estimates for this machine.

## Recommendation

**[I] Trial mbx first, Kache second; keep the Cargo fork as an experimental
alternative.** Both wrappers now address artifact reuse, concurrent duplicate
compiles, private incremental state, and storage cleanup. mbx fits Construct's
specific needs with a machine-wide CPU/memory budget, optional test scheduling,
managed per-checkout targets, and no persistent daemon. This is a fit judgment,
not evidence that it is faster than Kache.

The first pilot should retain Construct's repository-wide xtask mutex, wrap
nested Cargo invocations, and disable build-script execution replay until Git
version stamps are checked. It must also adapt the existing target-copy hook
before managed targets are enabled.

## What each tool actually changes

| Concern | Kache | mr-boxington | huacnlee/cargo |
|---|---|---|---|
| Build driver | Ordinary Cargo plus compiler/Cargo shims and background service | Ordinary Cargo plus shims and an agent that exits with each command | Replaces Cargo itself |
| Worktree state | Separate targets normally; shared content cache | Per-checkout managed targets, shared content cache | One repository build root, cross-project dependency root |
| Concurrent identical work | Per-key in-flight deduplication | Shared in-flight deduplication | Fine-grained build-unit locks |
| Machine resource scheduling | Memory-weighted permit pool; important incremental exception below | Independent shared CPU and predicted-memory budgets | Existing Cargo jobserver; no cross-command machine budget identified |
| Editing | Private adaptive incremental sessions; compatible donor seeding | Private learned incremental sessions; compatible donor seeding | Shared incremental state for eligible content-keyed crates/dependents |
| Disk | Content deduplication, cache GC, target cleanup policies | Content deduplication, managed-target and incremental-state budgets | Project/unit GC and shared-dependency budget |

**[V] Sources:** [Kache worktrees](https://github.com/kunobi-ninja/kache/blob/97d7b802a5f870ba9abfc645bd34609f70791a23/docs/getting-started/worktrees.mdx),
[Kache scheduling](https://github.com/kunobi-ninja/kache/blob/97d7b802a5f870ba9abfc645bd34609f70791a23/docs/how-it-works/limits.mdx),
[mbx architecture](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/how-it-works.md),
[mbx scheduling](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/scheduling.md),
[fork defaults](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/README.md#what-changes),
[fork jobserver implementation](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/src/compiler/job_queue/mod.rs#L33-L38).

### Kache

**[V]** This is substantially more than a cache for third-party rlibs. Current
Linux support includes eligible executables, test executables, build-script
compilation and execution, and native objects. It seeds compatible dependency
units into new targets and can seed independent incremental sessions on rustc
1.98+. Subsequent adaptive incremental compiles explicitly take **no scheduler
permit**; initial seeded compilation does. Unsupported invocations also pass
through. Consequently its scheduler is not a universal bound on every compiler
an agent can start. Its target runner can pace tests too; Unix initialization
configures that runner. See [limits](https://github.com/kunobi-ninja/kache/blob/97d7b802a5f870ba9abfc645bd34609f70791a23/docs/how-it-works/limits.mdx)
and [configuration](https://github.com/kunobi-ninja/kache/blob/97d7b802a5f870ba9abfc645bd34609f70791a23/docs/getting-started/configuration.mdx).

**[V]** Restoration prefers filesystem clones, with artifact-specific hardlink
or copy fallback. Writable metadata and seeded incremental files get private
inodes. On this machine's Btrfs, clones should be available if cache and targets
remain on a compatible filesystem. Logical target size is not exclusive disk
consumption. [Deduplication policy](https://github.com/kunobi-ninja/kache/blob/97d7b802a5f870ba9abfc645bd34609f70791a23/docs/deduplication.mdx).

**[I]** Kache is a strong alternative if fresh-target startup, its reporting, or
its broader native compiler support proves better in the real workload. The
service and launcher lifecycle add operational state worth testing. Its inspected
head includes a same-day fix for build-script launcher recursion and poisoned
entries; use a pinned release containing relevant fixes, rather than assuming a
README version and main have identical behavior.
[Fix commit](https://github.com/kunobi-ninja/kache/commit/97d7b802a5f870ba9abfc645bd34609f70791a23).

### mr-boxington

**[V]** mbx keeps Cargo responsible for planning and running tools, starts a
per-command cache agent, and uses a shared compiler permit pool. CPU and memory
budgets are separate, with memory estimates learned from measurements; these are
admission estimates, not OS memory limits. Test scheduling is **off by default**
and can be enabled with `MBX_SCHEDULER_TESTS=1` / `scheduler.tests = true`.
[Architecture](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/how-it-works.md),
[scheduling and test limits](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/scheduling.md).

**[V]** Managed targets are separate per checkout and exposed through a `target`
symlink. Check and Clippy use a separate check lane when eligible, while two
build/test commands sharing the same lane can still wait on Cargo's target lock.
Explicit target-directory choices are respected. Fresh-checkout dependency-unit
seeding requires Cargo **1.100+**; on the inspected local 1.99.1, compiler-cache
restores provide reuse instead. [Managed targets](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/managed-targets.md).

**[V]** Workspace edits get private learned incremental state; rustc 1.98+
can seed it from another checkout using clones or copies, never writable
hardlinks. Stable compiler processes using this state receive scoped
`RUSTC_BOOTSTRAP=1`, `-Zremap-cwd-prefix=.`, and an empty feature allowance.
Kache also uses restricted bootstrap for this capability; it is not unique to the
Cargo fork. Debuggers may need a source-directory mapping. mbx documents a
one-time discard of old incremental state to correct earlier hardlink sharing.
[Incremental behavior](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/incremental.md).

### huacnlee/cargo

**[V]** The fork moves intermediate builds into `~/.cargo/targets`, shares
registry/git dependencies across projects, content-keys eligible workspace
units, enables fine-grained locking, and performs unit GC. It deliberately
ignores `--target-dir`, `CARGO_TARGET_DIR`, `build.target-dir`, and
`build.build-dir` with warnings. Its installer replaces the rustup Cargo proxy
under `~/.cargo/bin`; background automatic updates are enabled unless disabled.
Nightly defaults also change code generation to Cranelift for local development
builds. [README](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/README.md).

**[V]** The source excludes every package with a build script from workspace
content sharing: `hash_files` returns `None` for it. Construct's main crate has
such a script, so it does not get the central advertised own-crate sharing
benefit. Crates reading checkout paths such as `CARGO_MANIFEST_DIR` can also be
bound to a checkout. [Eligibility and marker checks](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/src/compiler/shared_units.rs#L234-L244).

**[V]** Fine-grained locks avoid serializing the entire repository build root,
but eligible content-keyed units and dependents share profile incremental state
and a stable output symlink. The symlink's exclusive lock is held through rustc
execution, so distinct edits to the same crate/mode can still serialize there.
[Shared incremental layout](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/src/compiler/build_runner/compilation_files.rs#L387-L444),
[lock acquisition](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/src/compiler/shared_units.rs#L95-L109),
[lock lifetime](https://github.com/huacnlee/cargo/blob/6ebb89197f5b3a88b637e28672e6d39d67343401/src/compiler/mod.rs#L516-L524).

**[I]** This is the broadest maintenance/behavior commitment of the three. The
local active Cargo is Arch's `/usr/bin/cargo`, not a rustup proxy, so the documented
installer's effect cannot be assumed. Replacing the build driver, overriding
isolation flags, and changing several defaults is a poor first experiment when
compiler wrappers offer a narrower reversible path.

## Construct-specific findings

**[V, local inspection]** The main checkout was clean at `ceccb7e`, with 16
auxiliary registered worktrees. It has two workspace crates, stable Rust/Cargo
1.99.1, 24 logical CPUs, approximately 125 GiB RAM, and Btrfs with zstd
compression. No compiler cache or custom linker was installed. The dev profile
already uses line tables instead of full debug info; bundled SQLite adds native
compilation. See [Cargo.toml](../../Cargo.toml),
[rust-toolchain.toml](../../rust-toolchain.toml).

**[V]** Construct already serializes xtask across **all** worktrees using a lock
in the Git common directory and caps nested Cargo/test threads at eight here.
The outer `cargo run` that builds xtask occurs before that mutex. The helper
uses the `CARGO` environment value when spawning nested Cargo, so a shell alias
or wrapping only the outer command is not proof that the inner gates use the
new tool. [xtask helper and mutex](../../crates/construct-dev/src/xtask.rs#L30),
[alias](../../.cargo/config.toml).

**[V]** A checkout hook already reflink-copies main's target into fresh worktrees.
Thus a totally cold vanilla worktree is the wrong baseline. Sampled Btrfs output
reported main at 31.52 GiB total / 469.03 MiB exclusive and another target at
38.61 GiB total / 8.82 GiB exclusive. Those samples are not a complete
physical-space accounting; summing ordinary `du` outputs would overstate disk
use because shared extents appear repeatedly.
[Existing hook](../../.githooks/post-checkout#L14).

**[V]** The hook is incompatible with blindly enabling mbx managed targets:
`cp -a` copies main's target **symlink** rather than creating a private target.
mbx deliberately leaves a link to another live checkout's managed target alone,
as its explicit test confirms. Adapt or retire target copying **before** target
migration; otherwise new worktrees inherit the same target and Cargo lock.
[mbx regression test](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/crates/mbx/src/target_tests.rs#L187-L202).

**[V]** Construct embeds Git SHA and dirty status in a build script, watches
source/manifest and resolved Git HEAD/ref/reflog paths, and shells out to Git.
Both wrappers cache eligible script execution based on declared Cargo freshness
inputs; neither automatically traces all reads of arbitrary Git subprocesses.
The script does not declare the Git index even though `git status` depends on
it. This is an input-contract concern, not a demonstrated cache bug; plain Cargo
also relies on the script's declared inputs. [Local script](../../crates/construct/build.rs),
[mbx replay model](https://github.com/jdx/mr-boxington/blob/599e5b6e105d4006373ef2240c676026c6dab74f/docs/limits.md#build-script-execution-follows-cargos-freshness-inputs),
[Kache replay model](https://github.com/kunobi-ninja/kache/blob/97d7b802a5f870ba9abfc645bd34609f70791a23/docs/how-it-works/build-scripts.mdx).

## Proposed pilot and acceptance evidence

**[I]** Use two disposable worktrees and a pinned tool version; do not stack
cache wrappers. Keep current xtask serialization initially. Verify actual
nested compiler commands use the cache. Start with script execution replay
disabled (`MBX_BUILD_SCRIPT_EXECUTION=0` or `KACHE_BUILD_SCRIPT_CACHE=0`), which
still permits Rust and native object caching but also reruns dependency scripts.

Measure the existing hook-seeded baseline, a second same-revision checkout,
independent source edits, repeated edits, and concurrent checks/tests. Record wall
time, CPU/RAM peaks, time waiting on each lock, cache bypass reasons, and Btrfs
exclusive/shared bytes. Validate `construct manifest` against HEAD and dirty
state after edits, staging/unstaging, commits, branch switches, and worktree
creation; the existing verification doctor's stamp check is useful here.

Only after this passes should a separate change consider replacing the coarse
xtask mutex with measured global scheduling. Compiler scheduling alone does not
coordinate every non-Cargo gate or arbitrary subprocess. The pilot decides
whether mbx improves this repository over its already-reflinked baseline; vendor
warm-build numbers cannot answer that.
