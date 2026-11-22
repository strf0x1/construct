# 0064 — Optional tools are crates behind binary features

Status: accepted. 2026-11-21. Part of #271; builds on ADR 0056.

## Context

#271 wants a construct small enough to drop anywhere, and the freedom to
swap one tool's implementation for another and measure both. Cargo decides
what is compiled; `[tools] disabled` and the router only decide what the
model sees. A tool that lives in core is in every binary.

Two ways to make a tool optional:

1. `#[cfg(feature = "...")]` inside construct-core, around each tool module
   and its push in `Catalog`. Quick, but the `cfg`s spread: the router names
   `Group::Web`, `Group::Memory` and `Group::History`, the learner imports
   `SkillView`, and every combination needs CI.
2. Each optional tool becomes its own crate that implements `ToolSource`
   (ADR 0056). The binary turns it on with a feature, and the only `cfg` is
   in `crates/construct/src/cli/sources.rs`.

## Decision

- Optional tools are crates above construct-core, registered as a
  `ToolSource` in `sources.rs` under `#[cfg(feature = "<name>")]`. Each
  crate owns its dependencies, so a build without it compiles none of them.
  The first is `construct-tools-web` (`web_search`, `web_fetch`, `htmd`, and
  reqwest's `query`/`form` features), behind the `web` feature.
- The binary has two named bundles of features. `full` is the default and
  every release build. `lean` is the drop build:
  `cargo build --release -p construct --no-default-features --features lean`.
  Single features can be added on top (`--features lean,keyring`) to measure
  one at a time. `--no-default-features` alone is the bare build: core tools
  only. ADR 0051's env-only build is now any bundle without `keyring`.
- The router asks about and returns only groups the session's catalog has
  (`Catalog::groups`, `router::Input::groups`). A build without a tool crate
  never routes to its empty group or spends a Jev question on it.
- Features stay additive, as Cargo requires. Two implementations of one
  tool are two crates with two features that give the same wire name; the
  catalog's duplicate-name check refuses a build that turns on both. To
  compare behaviour rather than bytes, compile both in and pick one at
  runtime with config, so `bench.py task --arm` can run them side by side.
- A catalog built without the binary's sources (`build_without_sources`,
  the learner's) accepts a `[tools] disabled` name it lacks, since that
  name may belong to a source. `build_extended` still rejects typos.
  `construct manifest` and `construct route` now build with the sources, so
  they report what the binary runs.
- `text::prop`, the shared property-test strategies, is public behind
  construct-core's `test-support` feature, which tool crates take as a
  dev-dependency.

## Consequences

- Moving another tool out is mechanical when nothing outside `tools/` names
  it. `history` (`construct-tools-history`) and `job` (`construct-tools-jobs`)
  followed, behind the `history` and `jobs` features, both in `full`.
  `memory` and `skill` are entangled with the learning pass and the per-turn
  skill loader, and need a cut first.
- A tool whose name core's prose used to spell (`journal_query` in the
  sandbox's denials) is named by its source through
  `ToolSource::policy_tools`; without the source the sentence leaves it out.
- Without `jobs`, the bash tool's `background` still works and a finished
  job still wakes the agent, but its text says "the job tool reads it
  sooner or cancels it" about a tool the build lacks. Hiding `background`
  when nothing can cancel a job is a follow-up.
- A user config that disables a tool the build does not have
  (`disabled = ["web_search"]` in a bare build) is refused as an unknown
  tool. That is right for typos but awkward for a shared config across
  builds; revisit if baked configs (ADR 0050) hit it.
- The order of tool names in the manifest changes: source tools come after
  core's. Request order is unchanged, because definitions are grouped by
  `Group`.
- A runtime plugin (WASM, or MCP as today) would come in through the same
  `ToolSource` seam. It is not part of this decision.

## Evidence

Release builds on arm64-apple-darwin (opt-z, fat LTO, strip, panic=abort):

| build | tools | bytes | vs origin/main at 82f9078 |
|---|---|---|---|
| `full` (default) | 15 | 8,310,192 | 8,293,504 (+16,688, +0.2%) |
| `lean` (`--no-default-features --features lean`) | 15 | 6,472,848 | 6,472,656 env-only (+192) |
| bare (`--no-default-features`) | 13 | 5,959,248 | 6,472,656 env-only (−513,408, −7.9%) |

The web tools alone cost about 513 KB, most of it `htmd` and the HTML
parser it brings. `full` grew by 16 KB, probably from the router's group
filter and the crate boundary changing what gets inlined; that is within
this change, not attributed further.

With `history` and `jobs` moved out as well: `full` 8,310,288 (15 tools,
+96), `lean` 6,456,096 (12 tools), bare 5,942,496 (10 tools). Together they
cost 16,752 bytes: `rusqlite` stays for the journal, so only their own
code goes.

An earlier experiment that also removed memory, skill, history, job and
Deck's tools measured 5,692,256 B against 6,472,656 B for the env-only build
with every tool, so that ceiling is about 0.78 MB.
