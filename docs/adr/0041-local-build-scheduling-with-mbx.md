# 0041 — Delegate local build scheduling to mbx

Status: accepted for a local trial. November 4, 2026.

## Context

Parallel agent worktrees duplicate compilation and compete for machine
resources. Construct mitigated this with a repository-wide xtask lock, reduced
job/test-thread counts, niceness, and a checkout hook that reflink-copied main's
target. Those controls obscure a trial of a shared compiler cache/scheduler,
and the hook would copy mbx's managed target symlink into another checkout.

## Decision

Trial mbx 1.29.2 on the maintainer's development machine, with ordinary Cargo
commands routed through mise and a stable fallback shim. Each checkout retains
its own managed target. Remove the repository's copy hook and xtask's lock,
niceness, and fixed job/thread caps. Nested gates resolve Cargo through PATH.
CI continues to work with plain Cargo and does not require mbx.

Use the explicit `cargo run -q -p construct-dev -- check --quick` form locally:
this mbx version bypasses the initial helper build behind the xtask alias's
introduced `--`. Keep the alias for ordinary Cargo compatibility.

Keep build-script execution replay off while checking Construct's Git revision
and dirty stamps. Keep the independent improvements to debug-info size,
integration-test linking, and quick gate selection. Local resource and storage
budgets, trial evidence, and rollback are recorded in
[the trial note](../research/mbx-trial.md).

## Consequences

Independent worktrees can verify concurrently. Scheduling and reuse are the
frontend's responsibility; contributors using plain Cargo get Cargo's normal
resource behavior. Existing branches retain old xtask code until updated.
The local hook-path override prevents those branches from reactivating target
copying during the trial.

Normal development for approximately one day will determine whether this
improves responsiveness, waiting, repeated edits, and disk use. The setup checks
establish integration, not a performance win. Revert the trial if the daily
workflow regresses; reconsider the direct invocation when mbx supports this
alias fully.

## Evidence

- [Source comparison and eligibility limits](../research/rust-worktree-build-caches.md).
- [Trial setup, observations, evidence location, and rollback](../research/mbx-trial.md).
- Local quick checks passed while another process held the former xtask lock.
- Two disposable worktrees ran quick checks concurrently with separate managed
  targets; their logs and mbx explanations are retained in the trial directory.
- mbx's doctor confirmed active wrapping and Btrfs cloning without warnings.
- Additional end-to-end validation: 219 passed, one ignored, one ACP diagnostic
  assertion failed; its exact failure reproduced with mbx disabled. See the
  trial note and saved control logs. No runtime fix is included in this change.
