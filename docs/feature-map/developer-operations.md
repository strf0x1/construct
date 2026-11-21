---
id: developer-operations
status: verified
surfaces: [tui]
depends: [crates/construct-core/src/tools/developer, crates/construct-core/src/permission/operation.rs, crates/construct-tui/src/ask.rs, crates/construct-tui/src/prompt.rs, crates/construct-tui/src/turn.rs, crates/construct-tui/src/activity.rs, crates/construct-dev/tests/developer.rs]
verified_at: 40f7a7a
verified_deps: c471b68307736111
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
- `developer-operation-merge-commit` approves a staged merge resolution and records the merge parents.
- `developer-operation-linked-worktree` checks status, stages named files, and starts a merge of one exact commit in the current linked checkout; a plain `git` failure there ends with a sandbox note naming these operations.
- `developer-operation-deck-gateway` in a Deck VM task with a repository (`DECK_GITHUB_URL`), push, pr_create, pr_update and pr_review go through Deck's GitHub gateway instead of host authentication, and in Deck mode Construct shows no panel of its own: Deck's gateway decides each write. `issue_create` is refused there.

## How to get to it (user POV)

- Ask construct to create a GitHub issue with a repository, title, and body.
- Stage changes and ask construct to commit them with a message.
- After resolving a Git merge, stage the resolution and ask construct to commit it.
- From a linked worktree's root, ask construct to check status or stage named files. To start a merge, give it the full commit ID.
- Ask construct to push a named branch to a named remote.
- Ask construct to create a pull request with a repository, base, head, title, and body.
- In the permission panel, use page-up/page-down or home/end to review content,
  then choose allow, exact-operation session approval, or deny. Esc interrupts.

## Done when

- The panel names the operation and destination and exposes all review details.
- Allow commits precisely the staged content reviewed; deny leaves HEAD unchanged.
- Changes made to the staged index while waiting cannot replace approved content.
- A merge commit contains the reviewed parents and clears the worktree's merge state.
- Status shows a linked worktree's conflicts. Staging resolves the native index and leaves the other checkouts and branches intact.
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
- **Deck gateway.** Drive: `cargo test -p construct-dev --test it deck_github::` (a stub of Deck's gateway; no TUI panel exists in this mode). Expect: an incremental bundle for the task branch, a draft pull request, checks read back, a rewrite reported as rebase-onto, and every request head valid against the contract 1.2.0 schemas.
- **Resolve a merge.** Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/developer-merge-commit 120 40`. Expect: the approval shows the merge parent, then Git records both parents and the staged resolution on the integration branch.

## Gotchas

- Wait 600 ms after the panel appears before answering.
- Commits are unsigned and skip hooks. Pushes skip hooks and do not force.
- Finish a merge with an autostash in Git to restore the unstaged changes.
- Create and push the head branch before requesting PR creation; PR creation never pushes.
- Headless runs cannot acquire host-operation approval.
- An ambiguous remote result needs reconciliation before any repeat.
