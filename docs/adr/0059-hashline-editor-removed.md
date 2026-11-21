# 0059 — The hashline editor is removed; `replace` is the only editor

Status: accepted, 2026-11-21. Supersedes 0002, 0007 and 0018 (their editor
comparisons and the count-salt work concern code that no longer exists).

## Context

0002 kept `replace` as the default and hashline as a config arm; 0007 and 0018
measured and then built the count salt for hashline's stale-duplicate hole.
Across those runs and the later ones (#272, #100) hashline never beat
`replace` on the edit corpus, and it cost about 2.3k lines in core
(`tools/edit/hashline/`), a `[tools] editor` knob, a `[tools.hashline]` table,
a contract test file and bench arms, for a path no default exercised.

## Decision

`[tools] editor`, `[tools.hashline]` and `CONSTRUCT_TOOLS_EDITOR` are gone. The
catalog always builds `read`, `write` and `edit` from `old_string`/`new_string`
replacement. An old config that still sets them is ignored, since the config
tables do not reject unknown keys.

`Manifest.editor` stays and is always `"replace"`, so older results and
journals still parse and `bench.py compare` still finds the field. The
`hashline` manifest field and the `rust-replay-hashline-salt` task are removed.

## Consequences

- Core is smaller by the hashline module and its contract file; the file-tool
  contract tests no longer loop over editors.
- A different editor is a new `Tool` pair added through the catalog (or a
  `ToolSource`, ADR 0056), not a knob: bring a bench result with it.
- Old bench results that name hashline stay in `bench/results/` as history;
  nothing reads their `hashline` block except the manifest summary line.

## Evidence

Golden request snapshots are unchanged: the default was already `replace`.
