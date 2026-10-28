# 0023 — A verified feature goes stale when its code moves

Status: accepted. 2026-10-28.

## Context

`docs/feature-map/` says which features are `verified`: a recipe was driven
against the real TUI at the commit in `verified_at`. The `map` gate in
`cargo xtask check` checked that the map is well formed and names things that
exist, but not that a `verified` claim still holds. A change to
`tui/prompt.rs` left `permission-prompt` claiming "verified" for a panel
nobody had looked at since.

To notice, the gate has to know which files an entry's proof covers. The
options were:

- **Derive everything from the recipe directory.** The recipe is data
  (`model.json`, `drive.txt`, `check.sh`); it names no source files, so this
  alone would only catch edits to the recipe.
- **Record dependencies per recipe** (a file in each recipe directory). One
  entry often has several recipes (`slash-commands` has four), and the
  dependency is a property of the feature, not of one way to drive it.
- **A `depends:` list in the entry's frontmatter, plus its recipe
  directories.** Frontmatter is where the map already keeps machine-read
  facts (`status`, `surfaces`, `verified_at`), and the format keeps file
  paths out of the prose.

## Decision

- An entry declares `depends: [path, …]`: files or directories from the repo
  root that the feature's user-visible behavior lives in. It is required when
  `status: verified` and allowed (and checked to exist) on any status.
- An entry's dependencies are its `depends:` paths plus every recipe
  directory it names.
- The `map` gate fails, naming the entry, the moved paths and the commit
  count, when `git log --no-merges <verified_at>..HEAD -- <dependency>` is
  non-empty for a `verified` entry. The fix is to re-drive the recipe and
  bump `verified_at`, or to set `status: built`.
- `verified_at` names the commit the recipe was driven against, so the bump
  is its own commit that touches only `docs/feature-map/`: drive at the
  commit that changes the code, then record it. The entry file is not a
  dependency, so the bump never trips the check.
- Merges are not counted (their changes are counted in the commits they
  bring in). A commit merged in from main that touched a dependency does
  count: the tree that was driven did not have it.
- The git query is injected (`feature_map::Git`), like `is_commit`, so the
  check is unit tested without a repository.

## Consequences

- About a third of PRs will have to re-drive one or more recipes (~5 s each)
  or drop an entry to `built`. That is the intent: the map stays honest
  without anyone reading it.
- `depends:` is only as good as its author: a dependency left off the list
  goes unnoticed. Lists name the feature's own modules, not everything they
  import; a shared module like `tui/input.rs` belongs on every entry whose
  keys or commands it handles.
- A branch whose `verified_at` is a squash-merged commit no longer reachable
  from HEAD would count every commit since the fork point. This repo merges
  PRs with merge commits, so this does not arise; if that changes, revisit.
- The `map` gate's color claim now also accepts a color that one of the
  entry's own recipes asserts with `expect-style`, since driving the recipe
  proves it. Blended colors (the `ctx` meter) are computed, not `Rgb`
  constants.

## Evidence

At the time of the change (the repo's history starts 2026-10-19, so "the last
month" is nine days), with the dependency lists in this PR:

- 19 of 126 non-merge commits touched a dependency of some verified entry.
- 16 of 51 merged PRs on main touched one.
- All four verified entries were stale at the start of this PR:
  `permission-prompt` (3 commits since `ffa04de`), `scroll-copy` (2 since
  `c6b2a43`), `slash-commands` (15 since `ffa04de`), `status-line` (4 since
  `ffa04de`). Each was re-driven and bumped in this PR.
