# 0017 — Delegated children lock per tool call, like the session

Status: proposed. 2026-10-25. Partly supersedes 0014 (the child process is no
longer confined as a whole on the filesystem).

## Context

#66: a delegated child asks to read a path outside its definition, the user
approves, and the read still fails with EACCES. ADR 0014 starts
`construct __child` under the sandbox helper, so the whole child process sits
inside a Landlock ruleset built from `Policy::for_agent` at spawn time. ADR
0016's approval widens the child's in-process policy (`Policy::check_read`),
but a Landlock ruleset can never be loosened by the process it confines, so
`std::fs` still gets EACCES. It is not directory-specific: an approved file
read fails the same way; the existing test only asserted the child's summary
text. ADR 0016 already says a lent path takes effect "at its next delegation";
the prompt never said so, so the user is told yes and the call fails anyway.

The session's own agent doesn't have this problem because it is never
confined itself. ADR 0004 says why: "Landlock restriction is irreversible for
the process that applies it, so it cannot be applied in the agent itself."
The session locks each shell call in a fresh `construct __sandbox` and runs
file tools behind the in-process check, so an approval is just a wider policy
for the next check or the re-run (`tools/shell.rs`, `policy.widened`). ADR
0014 applied the lock to an agent anyway, as defence in depth for a child
running another model.

Options that were on the table:

1. **Don't ask from a child.** It returns an error naming `allow_read` in its
   definition. Small, but it drops ADR 0016's child-asks-parent flow.
2. **Restart the child with the widened lock**, resuming from the journal
   (`journal::project`). This works between turns, but the question comes in
   the middle of a tool call. Sibling calls would be killed partway through
   (the helper sweeps everything the child started), the pending call would
   need a "resume with pending tool calls" entry, state outside the journal
   (routed tool groups, session grants, scratch home) would have to be
   rebuilt, and long-running commands and hooks would die on every approval.
   "Allow once" would also turn into "allow for the rest of this delegation".
3. **The parent reads on the child's behalf.** That keeps the child's lock, but
   it's a new path for every file tool, and the child's shell still can't
   reach the file.
4. **Lock the child per tool call, like the session.** Chosen.

The deciding argument came from the user: permissions describe what
*construct* may do, not what one process may do. Someone who denies `.env` to
the session expects the child to be denied too, and someone who approves a
path expects the approval to work for the call that asked.

## Decision

- The child process is no longer confined on the filesystem. The helper that
  starts `construct __child` applies a ruleset that handles network and
  signals only (`AccessNet` when the definition has `network = false`, plus the
  endpoint port, and `Scope::Signal`), and does not handle `AccessFs`. Landlock
  allows a ruleset that handles a subset of access types, so this is one
  ruleset with fewer handled rights, not a second mechanism.
- Inside the child, tools are confined exactly as in the session. Shell calls
  each run in a fresh `construct __sandbox` built from the child's current
  policy, so a re-run after approval gets the widened lock. File tools go
  through `Ctx::can_read`/`can_write` and `Policy::check_*`. The policy is
  still `Policy::for_agent`, sent in `start`: containment is still subtraction
  from the parent (ADR 0014). Only where that policy is enforced has moved.
- An approval makes the call that asked succeed, file or directory, for
  once / session / always (ADR 0016's meanings, unchanged). The "next
  delegation" caveat in ADR 0016 is dropped.
- The user's decisions apply to construct as a whole. The parent's `Perms`
  answers a child's question, and a path the user denied, or a protected path
  (`permission::grants::protected`), is denied to parent and child alike.
- Unchanged from ADR 0014: the scrubbed environment and endpoint-only
  credentials, the scratch `CONSTRUCT_HOME`, no MCP and no memory/skill-write/
  history groups, depth capped at one, process isolation for crash and cancel,
  and the helper's subreaper sweep.
- File tools close the check-then-open race. Opens go through `openat2` with
  `RESOLVE_BENEATH` relative to the granted root (plus `O_NOFOLLOW` where a
  single file is meant), in the session and the child alike, so the kernel
  rejects a path that escapes its root at the moment of opening. Kernels
  without `openat2` (before 5.6) fall back to today's canonicalize-and-check.

## Consequences

Easy: an approval does what the panel says, in a child as in the session.
Long-running commands and hooks in a child are never killed by an approval.
One permission model to reason about, test and document.

Hard, and accepted: a child's file tools lose their kernel backstop. What the
whole-process lock used to catch silently now depends on construct's code,
exactly as it already does for the session:

- **The symlink race.** The path is checked, then opened by name, and a
  parallel shell call swaps in a link between the two. `openat2` above closes
  this.
- **A forgotten check.** A code path that touches a file without asking the
  policy: a new tool, a walker following a link, a temp file during an atomic
  write. Mitigation: file access in tools goes through one module that
  takes the policy, and a contract test walks a symlink farm through every
  file tool.
- **A bug in the check.** Prefix matching (`/home/b` vs `/home/b2`), `..`,
  case. Mitigation: contract tests on `Policy::check_*`.

A child's shell commands are still kernel-confined per call, and the child
process still can't open TCP beyond its endpoint port or signal processes
outside its scope.

This rules out treating a child as more trusted *or* less trusted than the
session on the filesystem: it gets the session's enforcement, narrowed by its
definition.

Revisit when #79 lands. A locked, stateless file-tool worker would give file
tools kernel confinement in the session and the child alike, and could be
restarted freely on approval because it holds no state. Revisit also if a
child needs a tool whose file access can't go through the policy.

Open, to settle before this is accepted:

- Whether the session's own grants, given before the delegation, flow down to
  a child without asking, when the path is outside its definition. This ADR
  only fixes that the user's answers and denials bind both.
- Whether a child's prompt should still offer "once", now that once works.
  It does, so the default is yes.

## Evidence

#66 and draft PR #75: `approving_a_childs_directory_read_gives_it_the_listing`
reproduces the failure (currently `#[ignore]`d), and
`approving_the_sessions_own_directory_read_once_gives_the_listing` shows that
the session is unaffected. The decision was discussed on #66 (comment
2026-10-25). The follow-up worker is #79.

For acceptance, the implementing PR should show:

- The #75 test un-ignored and passing.
- `a_construct_childs_blocked_read_asks…` asserting the child's own read
  result.
- New e2e cases:
  - a child shell command re-run after approval succeeds;
  - a protected path is refused to a child without a prompt;
  - a child with `network = false` still can't open TCP beyond its endpoint;
  - a symlink swapped in between check and open is refused.
- Tier 0 ok. Tier 1 `bench.py tools`, with the `openat2` path compared
  against main.
