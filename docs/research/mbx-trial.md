# mbx development trial

Started November 4, 2026 (America/Denver). This is a local development trial;
normal development for roughly a day is the performance evaluation. The survey
is in [rust-worktree-build-caches.md](rust-worktree-build-caches.md).

## Changes

- Install and pin mr-boxington 1.29.2 with the existing mise installation.
  Keep Arch Rust/Cargo 1.99.1; do not install a second Rust toolchain.
- Activate the global mise Cargo wrapper, the stable mbx Cargo shim, and
  rust-analyzer's separate check command. Expose the stable shim
  and its mbx-target companion through ~/.local/bin, already on PATH, so nested
  Cargo aliases still reach mbx after mise removes its own dispatch paths.
- Use `cargo run -q -p construct-dev -- check --quick` for local verification.
  mbx 1.29.2 bypasses the initial helper build behind the xtask alias because
  it introduces an application separator. Keep the alias for plain-Cargo CI;
  the direct invocation schedules/caches startup and restores a clean launch
  environment before the nested checks run.
- Remove xtask's Git-common-directory lock, niceness, per-command job cap,
  test-thread cap, and automatic checkout-hook installation. Resolve nested
  Cargo through PATH instead of the underlying executable in CARGO.
- Delete the checkout hook that copied main's target. During this local trial,
  core.hooksPath names an empty directory in the main Git directory so an old
  worktree cannot reactivate its old copy of the hook. No other hooks existed.
- Keep line-table debug info, consolidated integration-test binaries, and
  --quick test selection. These reduce intrinsic work independently of mbx.
- Keep each worktree's target private. mbx adopts an existing target when it
  builds that checkout and restores eligible outputs from its shared store.

## Local policy

The user-level configuration is ~/.config/mbx/config.toml:

- Shared compiler pool: 24 logical CPUs minus 4 reserved CPUs, 64 GiB memory.
- Test scheduling enabled, including nested Cargo test commands.
- One 100 GiB logical collection budget for managed build data.
- Plain display and savings messages; local cache only, no remote service.
- Build-script execution replay disabled for the trial. Scripts still execute
  normally, while eligible compiler and native-object work can be cached.
  The checked-in .mbx.toml repeats this policy for new worktrees.

The scheduler admits work against estimates; its memory budget is not an OS
limit. mbx's storage budget is logical and can temporarily be exceeded by active
or protected builds. Btrfs shared blocks make physical consumption different.

## Evidence and follow-up

Original files, tool versions, hardware details, the initial worktree list,
build logs, timings, and mbx snapshots are saved outside the repository at:

`/home/brandon/.local/state/construct-mbx-trial/latest/`

The initial plain-Cargo build completed in 16.664 seconds and recompiled several
crates. This is a recorded starting observation, not a comparable steady-state
speed result. The setup doctor reported zero warnings or failures and confirmed
Btrfs cloning. Startup diagnostics and test evidence are kept with the logs.

Validation passed: quick checks in the main checkout, two simultaneous quick
checks in disposable worktrees, and the same concurrent test using the direct
invocation recommended above. The former Git-common xtask lock was held by the
test driver throughout the concurrent checks; both completed anyway. Their
managed targets resolved to different directories. mbx's recorded last build
identified the nested test command and reported real compiler-cache hits.
These are integration checks, not a representative before/after benchmark.

The additional construct-dev end-to-end suite produced 219 passes, one ignored
case, and one failure: `acp::acp_stdout_is_only_json_rpc_and_diagnostics_are_stderr`
expected a keyring diagnostic that was absent. The exact test failed identically
with mbx enabled and with `MBX_DISABLE=1` using plain Cargo. Its runtime behavior
was not changed in this trial; the failure remains unresolved and is recorded
in `end-to-end-tests.log`, `mbx-targeted.log`, and `plain-cargo-targeted.log`.


After a day of ordinary work, compare current `mbx stats --json`,
`mbx stats --scheduler --json`, and `mbx cache stats --json` with the handoff
snapshots in that directory. For a build that feels slow, run `mbx explain
--last` from that checkout. Report desktop responsiveness, waiting, first
worktree startup, repeated edits, and any failures. Do not treat estimated
compiler time saved as measured wall-clock improvement.

Fresh worktrees created from the local trial commit inherit the source changes.
Existing branches retain their old xtask code until updated; their old locks
and caps therefore do not measure the new setup. Their checkout-copy hook is
already disabled repository-wide. The main checkout's changes are local until
published through the usual review workflow.

## Rollback

Revert the local trial commit to restore the source-level scheduling and hook
behavior. Its hash is saved in the evidence directory after verification.
Keep subsequent unrelated development changes.

Run `mbx setup --global --uninstall` to remove automatic mise wrapping and the
rust-analyzer override it created. Remove the two trial symlinks ~/.local/bin/cargo and ~/.local/bin/mbx-target,
then remove its mr-boxington tool pin if uninstalling completely.
The original mise config and any pre-existing rust-analyzer/mbx configs are
saved under `user-before/`; inspect them before restoring if settings changed
since the trial. The original mbx and rust-analyzer config files did not exist.

Restore `core.hooksPath` to `.githooks` only after restoring the hook and
ensuring main's target is a real private directory. Managed target symlinks
can continue working with plain Cargo on this reflink-capable machine; do not
restore the old copying hook while main's target is still a managed symlink.
To turn a managed target back into a normal directory without discarding its
outputs, stop builds, remove only the symlink, then move its referent back to
that checkout's target path. Cached data need not be deleted to disable mbx.
