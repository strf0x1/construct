---
id: delegation
status: built
surfaces: [tui, cli]
---
# Delegation

With agents defined under `[agents.<name>]` in `config.toml`, the model can hand a self-contained task to one of them with the `delegate` tool. The child runs as its own sandboxed construct process with its own tools; the parent's transcript shows a `delegate <agent>` line while it works. When the child reaches past its sandbox, the user gets the same permission panel, badged `subagent · <agent>` in lavender, without the always-allow choice when the path is one the parent couldn't reach either. The child's summary comes back to the parent as the tool result.

## Sub-features

- `delegation-run` the parent's delegate call runs the child and returns its summary.
- `delegation-child-prompt` a child's blocked access asks the user, badged as a subagent.
- `delegation-cancel` esc during a delegate call stops the child and anything it started.
- `delegation-headless` `construct run` delegates the same way, with no prompts (blocked access is denied).

## How to get to it (user POV)

- Define an agent in `~/.construct/config.toml` (`[agents.scout]` with `description`, `tools`, optional `model`, `prompt`), then ask construct to delegate to it ("delegate to scout: …") in the TUI.
- Headless: `construct run "delegate to scout: …"`.

## Done when

- The transcript shows `delegate scout`, then the parent's reply built from the child's summary.
- A child's blocked read shows `PERMISSION` with `subagent · scout` in lavender; allowing it lets the child's read through.
- The journal holds the child's rows under its own session id, including its `access` row, and the parent's `tool_end` for `delegate` names that session.

## Driving it

Preconditions:

- `setup.sh` writes `[agents.scout]` (tools `["read"]`) to the scratch `config.toml` and `{outside}/f.txt` containing `scout-data`. Model steps: parent delegates; child reads `{outside}/f.txt`; child answers `it holds: scout-data`; parent answers `the scout reported`. Recipe: `.agents/skills/verify-construct/recipes/delegation`.

- **Delegate.** Drive: `type ask scout`, `key enter`, `wait "PERMISSION" 10000`. Expect: `delegate scout` in the transcript and the panel open.
- **Subagent badge.** Drive: `sleep 600`, `expect "subagent · scout"`, `expect-style "subagent · scout" fg=#bb9af7`, `image {evidence}/child-prompt.png`. Expect: the lavender badge, no always-allow row.
- **Allow.** Drive: `type y`, `wait "the scout reported" 10000`. Expect: the parent's reply.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/delegation`. Expect: `ok: the child's approved read succeeded` — `journal.txt` has an `access` row with `"decision":"once"` under the child's session id, and the child's `tool_end` for `read` has `"ok":true`.

## Gotchas

- **This recipe fails today, which is why the status is `built`.** The panel appears and the decision is recorded, but the child's approved read still gets `Permission denied (os error 13)`, for files as well as directories (issue #66). The screen and the child's scripted reply both look fine; only `check.sh` catches it. Re-run after #66 lands and mark `verified`.

- Parent and child share the scripted model's steps in request order: parent call, child steps, parent reply.
- A child's key is only the secret its definition (or the parent's `api_key_secret`) names; with only `OPENROUTER_API_KEY` set, a real-model child gets a 401 (issue #65).
