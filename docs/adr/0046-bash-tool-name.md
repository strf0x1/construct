# 0046 — The shell tool is called `bash`; `shell` stays an alias

Status: accepted, 2026-11-18.

## Context

construct's command tool was called `shell`, but it only ever runs bash
(`tools/shell.rs` starts `/bin/bash`, falling back to `/bin/sh`), and its
description already says "Run a bash command". Models trained on other
harnesses expect `bash`: in the paired SWE-bench run for #236 a model called
`bash` and got "unknown tool bash", a wasted round.

The old name is recorded in places a rename must not break: `[tools]
disabled` and `[agents.*] tools` in users' `config.toml`, every journal
written so far (`tool_start` and `tool_end` rows, the assistant messages a
resume replays), and the bench readers that count shell calls. After a
rename, models that saw the old name in a resumed history will also call
`shell`.

The ACP contract (`strf0x1/sprawlcommons-acp`, pinned by `CONTRACT.lock`)
does not name tools: a call reaches Deck as ACP kind `execute` with a title
built from its arguments, so the contract does not change.

## Decision

- `Shell::NAME` is `bash`. It is the only name the model is offered, in tool
  definitions, the system prompt and other tools' prose, all of which name
  it through the constant.
- `Shell::ALIASES` (`["shell"]`) lists the old name. `tools::canonical`
  maps an alias to the current name, and is applied only after an exact
  lookup fails, so a tool that really has that name (a custom
  `search_name`) still wins.
- An alias is accepted wherever a name comes from outside the build:
  `[tools] disabled` (disabling `shell` disables `bash`), `[agents.*]
  tools`, and `Catalog::get`, which is behind dispatch, kind, display,
  readonly and group lookups. A model's call to `shell` therefore runs the
  bash tool rather than failing, and a resumed history that says `shell`
  needs no rewrite. Journals are not migrated: rows keep the name the model
  used.
- A disabled tool is unknown under either name.
- Bench readers match both names through `journal.SHELL_TOOLS`.
- Aliases are never removed silently. A new rename adds a row to
  `tools::RENAMED` and an `ALIASES` constant in that tool's `impl`.

## Consequences

Existing configs, agent definitions and journals keep working with no
migration. The journal now has both names for one tool, so anything that
reads it by name (scorers, `journal_query` users, bench scripts) must match
both; `journal.SHELL_TOOLS` is the bench's one place for that.

The job kind in `<job kind="shell">` and the scripted model's abstract
`"call": "shell"` step are separate vocabularies and did not change.

The tool's name and the prompt lines that name it are tool wording, a tier
2 change: real-model behaviour should be measured (`bench.py task`) before
this is judged an improvement rather than only a fix for the unknown-tool
round. Revisit if the alias is seen to confuse a model (calling both names
in one session) or if a provider rejects history that names a tool missing
from the request.

## Evidence

- The unknown-tool round in the #236 paired SWE-bench run.
- e2e `an_old_config_that_disables_shell_still_disables_bash`; contract
  tests `the_shell_tools_old_name_still_reaches_it`,
  `a_call_by_the_old_name_runs_the_shell_tool` and the `shell` row in
  `acp_kinds`; `delegate.rs`'s sleeper agent still lists `tools = ["shell"]`.
- Golden request snapshots re-recorded for the new name and prompt wording.
