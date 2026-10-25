# 0016 — Permission prompts: ask the user, remember the answer

Status: accepted. 2026-10-22.

## Context

#2: when the sandbox (ADR 0004) blocked a path, the model got an error naming
a config key and the user got nothing: a sibling repo or `~/.aws/config` meant
a manual config edit and a restart. #33 (ADR 0015) added a second kind of
question for the same person: a delegated ACP agent asking
`session/request_permission`. Both are decided in the middle of a running tool
call, in a process that may not be the one with the terminal.

## Decision

- One channel for both (`permission/`). A question is a `Request` (a path a tool
  was refused, or an ACP agent's tool call). Whoever can reach the user
  implements `Ask`: the TUI shows a panel; headless gets `NoAsk`, which says
  no, so `construct run` and `construct acp` behave as before. `Perms` (one per
  agent, on `Ctx`) checks what is already granted, asks only if it must, and
  applies the answer.
- Answers: **allow once** (only that check passes; the next call asks again),
  **for this session** (kept in memory), **always** (written to `config.toml`),
  **deny**. A path request may be widened from the file to its folder (Tab).
  Esc interrupts the turn and the question goes with it: the asking call
  races the turn's cancel, so nothing is left waiting.
- "Always" for the session's own agent is `[sandbox] allow_read`/`allow_write`.
  For a delegated agent it is that agent's `[agents.<name>]` `allow_read`/
  `allow_write`, and only when the session itself could reach the path
  (ADR 0014: a definition stays inside the parent's sandbox); beyond that the
  user can lend the path for the session, not write it. The file is edited in
  place (`toml_edit`), never re-serialized.
- The file tools ask before they run (`Ctx::can_read`/`can_write`). A shell
  command can't: Landlock shows up only as EACCES in its output. So a failed
  command's output is read for the paths it was refused (`permission::paths`),
  and if the policy really doesn't allow them the user is asked whether to
  **run the command again** with them added (at most twice per call). The panel
  says that approving re-runs it.
- Never offered, never in a session grant, never inside a folder grant:
  `~/.ssh`, `~/.gnupg` and the bench key files. A path typed into `config.toml`
  still works; a prompt is not a way in (`permission::grants::protected`).
- A construct child (JSONL) asks its parent: `Event::Ask` up, `ToChild::Answer`
  down. The parent decides, remembers and writes, because its home and config
  are the real ones; the child's `Always` is returned to it as `Session`. What
  was lent to an agent for the session is added to its policy at its next
  delegation, and to the call that asked at once (ADR 0017). A child over ACP transport has no question channel yet and keeps
  denying; that, and `construct acp` asking its client, are separate decisions.
- ACP agents: `[agents.x] on_permission` is now `ask` (default), `reject` or
  `allow`; `ask` with nobody to ask is a reject, which is what the default was
  (ADR 0015). `allow_kinds = ["read", ...]` answers those ACP tool kinds with an
  allow without asking, whatever `on_permission` says. Session and "always"
  remember a kind (or an exact title when the agent sent none), for that agent.
  The answer is still always one of the offered options or `cancelled`
  (ADR 0015); the agent's own `allow_always` is not what "always" means here.
- Journaled: `Event::Access` for a path decision (not for a headless denial,
  which nobody decided), and `Event::Permission` gained `scope` (`policy`,
  `config`, or the user's answer). `Event::Ask` is wire only.
- The TUI prompt takes over the bottom of the screen and every key, ignores
  keys for 450 ms after it appears, and is colored by consequence (green allow,
  amber writes config, red deny; border pulses; main agent blue, a subagent
  lavender).

## Consequences

Easy: a blocked path is a keypress, and the choice survives the session if the
user wants. A `--allow-read` flag is not needed: config already covers scripts.
Hard: a rerun repeats a command's side effects up to the point it failed; the
panel says so, and the command text is shown. The path heuristic for shell
output is prose parsing: a miss gives the model the plain sandbox hint, as
before. Revisit if models use an up-front escalation field instead (Codex), or
when a child over ACP should be able to ask.

## Evidence

PR for #2. Tier 0: `cargo xtask check` ok, golden snapshots unchanged (the
model's requests and the first frame are the same). Tier 1 results are in the
PR description.
