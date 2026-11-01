---
id: developer-operations
status: verified
surfaces: [tui]
depends: [crates/construct/src/tools/developer, crates/construct/src/permission/operation.rs, crates/construct/src/tui/ask.rs, crates/construct/src/tui/prompt.rs, crates/construct-dev/tests/developer.rs]
verified_at: 48a9438
---
# Approved developer operations

Ask construct to file an issue, commit staged changes, push a branch, or open
a pull request. A permission panel shows the concrete operation and its
content before construct uses host authentication. Each operation needs its
own approval. Ordinary shell commands keep their sandbox restrictions.

## Sub-features

- `developer-operation-review` shows the repository, content, destination, and host execution details.
- `developer-operation-scroll` page-up/page-down and home/end reveal long approval content while choices remain visible.
- `developer-operation-once` allows the prepared operation once.
- `developer-operation-session` remembers only the identical operation for this session.
- `developer-operation-deny` refuses execution.
- `developer-operation-git-state` changed staged content requires a new approval; pushes use the captured commit.

## How to get to it (user POV)

- Ask construct to create a GitHub issue with a repository, title, and body.
- Stage changes and ask construct to commit them with a message.
- Ask construct to push a named branch to a named remote.
- Ask construct to create a pull request with a repository, base, head, title, and body.
- In the permission panel, use page-up/page-down or home/end to review content,
  then choose allow, exact-operation session approval, or deny. Esc interrupts.

## Done when

- The panel names the operation and destination and exposes all review details.
- Allow commits precisely the staged content reviewed; deny leaves HEAD unchanged.
- Changes made to the staged index while waiting cannot replace approved content.
- A push sends the reviewed commit, even if the branch moves while waiting.
- The journal records the approval decision and the real tool result.

## Driving it

Preconditions:

- Baseline state with git installed. The `developer-operations` recipe creates
  a temporary repository with one staged change; no real remote is used.

- **Review.** Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/developer-operations`. Expect: a host-operation permission panel, a green selected approval, and a red deny choice.
- **Scroll.** Drive: `key pgdn`, `key pgdn`, `snap --style approval-end`, `key pgup`, `key pgup`. Expect: the last review details and the approval choices remain visible.
- **Approve.** Drive: `type y`, `wait "developer operation done" 15000`. Expect: the panel closes and the tool returns the new commit ID.
- **Confirm effects.** Drive: the recipe's `check.sh` and `cargo test -p construct-dev --test it developer::`. Expect: actual committed file contents match the reviewed change, and the journal records approval and success. The tests also cover denial, index substitution, linked worktrees, and pushing a captured commit to a local remote.

## Gotchas

- Wait 600 ms after the panel appears before answering.
- Commits are unsigned and skip hooks. Pushes skip hooks and do not force.
- Create and push the head branch before requesting PR creation; PR creation never pushes.
- Headless runs and delegated children cannot acquire host-operation approval.
- An ambiguous remote result needs reconciliation before any repeat.
