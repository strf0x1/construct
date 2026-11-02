# 0031 — A feature-map proof is checked by content, so it survives a squash

Status: accepted. 2026-11-01.

## Context

ADR 0023 made a `verified` entry go stale when `git log --no-merges
<verified_at>..HEAD -- <dependency>` is non-empty, and assumed PRs merge with
merge commits. `main` squash-merges. A PR that re-drives a recipe sets
`verified_at` to a commit on its own branch; the squash leaves that commit
out of `main`'s history, so the squash commit itself counts as a change and
`main`'s `map` gate goes red after the merge (#140 and #142, repaired by #146;
#150 would have done it again). Once the branch is deleted, a fresh clone
does not have the commit at all, and the gate reports `verified_at` as "not a
commit".

The options were:

- **Compare the dependencies at `verified_at` with HEAD by content.** Holds
  while the commit is fetchable, fails again once the branch is gone.
- **Merge such PRs with a merge commit, enforced in CI.** Needs a rule people
  remember, or a check on how a PR will be merged, which CI cannot see.
- **Record a fingerprint of the dependencies next to `verified_at`.** The
  proof travels with the entry, whatever happens to history.

## Decision

- A `verified` entry records `verified_deps`: a 64-bit FNV-1a hex digest of
  each dependency's git object id at `verified_at` (`path oid` lines, recipes
  then `depends:`, `-` for a missing path). Computed from git objects, so it
  is the committed tree, not the working copy.
- The entry is stale when the fingerprint at HEAD differs from
  `verified_deps`. When `verified_at` is in the repo, `verified_deps` must
  equal the fingerprint there, and a stale message names the moved paths and
  their commits; when it is not, the fingerprint alone decides.
- `verified_at` may name a commit not in the repo only if `verified_deps` is
  set and it looks like a commit id. It stays required: it says which tree was
  driven while it can still be checked out.
- Nobody computes the digest by hand. With `verified_deps` missing the gate
  fails with the value at `verified_at` (`verified_deps: <hex>`), which the
  author copies, so recording a verification is the same bump commit as before
  with one more line.

## Consequences

- A squash merge, a rebase, or a deleted branch no longer turns `main` red.
- A change and its revert leave the entry proven: the driven tree is back.
- Changing an entry's dependency list changes the fingerprint, so adding a
  `depends:` path needs a re-drive (or recording the value the gate prints at
  `verified_at`, if that commit already had the path as it is now).
- After a squash the message cannot name which path moved or how many
  commits; it says the dependencies changed since `verified_at`.
- FNV-1a is not collision resistant; the digest guards against honest drift,
  not against someone forging a proof, which they could do by editing the
  entry anyway.

## Evidence

`feature_map::git::tests::a_verification_survives_a_squash_merge_but_not_a_later_change`
builds a repo, verifies an entry on a branch, squash-merges it, deletes the
branch and prunes the commit: the gate passes, and a later edit to a
dependency fails it. All nine verified entries got `verified_deps` from the
gate's message at their existing `verified_at`.
