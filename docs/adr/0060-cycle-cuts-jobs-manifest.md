# 0060 — Config types for jobs live in `config`; `Manifest` takes tool names

Status: accepted, 2026-11-21. Follows 0057.

## Context

After #303 and ADR 0057, `construct-core` had only two two-way pairs left,
both intended (`config <-> models`, `config <-> providers`). The work map's
code graph still showed one strongly connected component of 16 modules:
`compact`, `config`, `context`, `endpoint`, `event`, `jobs`, `journal`,
`llm`, `manifest`, `mcp`, `memory`, `models`, `permission`, `providers`,
`sandbox` and `tools`. The pair ratchet counts only two-way pairs, so the
longer loops were invisible to it.

Removing edges one at a time from that component (`construct-dev
code-graph` at `18932c4`) showed that the ADR 0057 pairs were not what held
it together. Cutting both of them, in either direction, left all 16
modules connected. Two edges with three references between them did hold it:

| Edge | Site | Names |
|---|---|---|
| config → jobs | `config.rs:51`, `config/tools.rs:25` | `JobsConfig`, `ShellConfig` (field types) |
| manifest → tools | `manifest.rs:6` | `Catalog` (argument of `Manifest::new`) |

`config` named two plain-data structs defined in `jobs`, and `jobs` reaches
back to `config` through `event` and `sandbox`. `Event` carries a
`Manifest`, and `Manifest::new` took a `&Catalog` only to call `.names()`,
which made the event vocabulary depend on the whole tools layer.

## Decision

- `JobsConfig` and `ShellConfig` live in `config/jobs.rs`, the same shape
  `[tools]` and `[learn]` already have: a module reads its settings from
  `config`, and `config` never names a type from a module above it.
  `config`'s `may_use` no longer lists `jobs`; `jobs` may use `config`.
- `Manifest::new(cfg, tools: Vec<String>)` takes the tool names. `manifest`'s
  `may_use` no longer lists `tools`.

`event → manifest` was the other one-reference cut. It was not taken, because
it would change `Event`, which is additive-only and journaled.

## Consequences

- The component is down to `config`, `endpoint`, `models` and `providers`.
  ADR 0057 described it as `config`, `models` and `providers`, but `endpoint`
  is in it too: `config → providers → endpoint → config` (`endpoint.rs:9`
  names `Config`). Read 0057's cycle with `endpoint` included. Its decision
  stands.
- `event`, `journal`, `llm`, `jobs` and `tools` are no longer in a cycle with
  `config`, so the planned split of `event`/`journal` into their own crate
  (#274) no longer runs into one.
- No behaviour, wire shape or journal row changes. Golden snapshots are
  unchanged.
- The pair ratchet still can't see a cycle with no two-way pair. A ratchet on
  the size of the largest strongly connected component would catch one, and
  would be the natural follow-up.

## Evidence

- `construct-dev code-graph` before (`18932c4`) and after this change, with
  strongly connected components computed from its edges: one component of 16
  modules before, one of 4 after. Pairs are unchanged (`config <-> models`,
  `config <-> providers`).
