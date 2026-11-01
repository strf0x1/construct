# 0030 — Approve authenticated developer operations

Status: accepted. 2026-10-31.

## Context

Issue [#142](https://github.com/strf0x1/construct/issues/142) exposed a gap in
path approvals. Allowing `~/.config/gh` does not give a sandboxed process
access to the host keychain or restore scrubbed authentication variables.
Approving paths one at a time also obscures the action the user intended.

ADR 0016 remains the boundary for ordinary file and shell tools. Those tools
must not gain host credentials just because an issue, commit, push, or pull
request was approved.

## Decision

A dedicated `developer` tool prepares a concrete operation, asks through the
existing permission channel, then executes fixed arguments on the host.
The optional `developer` tool group is discoverable through `enable_tools`.
The tool has four operations: `issue_create`, `commit`, `push`, and
`pr_create`. It accepts no shell command, executable override, or environment
override.

The approval shows the operation, destination, content, executable identity,
and working directory. Long content can be reviewed with page-up/page-down
and home/end while choices stay visible. Once permits that prepared call;
session permits only the identical prepared plan. Changed content,
destination, executable, or git state needs a fresh decision. Grants do not
cross operation types, persist to config, or widen the shell sandbox.
Delegated children cannot acquire host-operation grants. Headless execution
denies them.

GitHub writes use `gh api --hostname github.com` with fixed issue or pull
request endpoints and JSON input held in memory. Pull request creation
cannot push a branch. The executing process can read its host credential
environment and credential store; credentials are never returned to the
model. Results expose the created URL and classified errors instead of raw
authentication diagnostics.

Commits capture the staged tree, branch, parent, and message, display the
staged diff, and recheck state after approval. A private Git context copies
the index and shares the captured object and reference storage. It loads no
repository or host-global configuration. This excludes configured hooks as
well as hook files; setting `core.hooksPath` alone does not exclude configured
hooks on recent Git. Preparation cannot launch a lazy fetch or update the
source index.

`commit-tree` creates an unsigned commit; `update-ref` compares the captured
parent before updating the branch. Linked worktrees use their actual git
directory. The private context supports files and reftable reference storage.

Pushes capture a commit ID, destination URL, and full destination branch
reference. They run from a private git context so repository configuration
cannot select host commands or credential helpers. Host authentication and
transport helpers remain trusted dependencies of this approved operation.
Pushes skip hooks and never force an update.

Host-runner scratch lives in construct's protected home, separately from the
shell's writable temporary directory. The runner resolves executables outside
sandbox-writable roots, records their digest, rechecks them, and uses an
environment allowlist. This protects against model-written binaries and git
environment injection. The user-installed executable and its host runtime
dependencies remain trusted.

Remote writes leave a durable record before execution. An ambiguous result
does not trigger a replay, including after restart. The user must reconcile
the remote result before clearing an unresolved record. Operation decisions
are journal events; ordinary tool start/end events record inputs and results.

## Consequences

One approval covers the filesystem, keychain, helper, and network dependencies
of a concrete action. Issue creation grants no permission to push, and a
commit grants no permission to publish it.

This deliberately does not support arbitrary authenticated CLIs, enterprise
GitHub hosts, signing, repository hooks, force pushes, local-file push
destinations, detached-HEAD commits, or persistent broad
developer grants. Supporting those requires a defined operation boundary.
Natural-language user instructions are context for the model, not a way for
the model to manufacture a permission grant.

Initial commits use an empty parent tree and an absent-ref comparison;
detecting that absence safely requires Git 2.46 or newer. Existing-branch
commits keep the ordinary revision-resolution path.

Credential injection was rejected because the receiving command can read
the credential and still needs network and helper permissions. A host shell
escape was rejected because binding arbitrary shell parsing and subprocess
behavior to the reviewed action is harder than providing fixed operations.

## Evidence

Issue #142 documents the host-authentication failure. Tier 0 Rust gates,
seven developer-operation e2e tests, 23 existing permission e2e tests, and 11
golden snapshot tests pass. The optional group adds 106 bytes to the plain
first request, with the same seven tools enabled; snapshots were regenerated
with `cargo xtask snapshots`.

Tier 0 drive: `developer-operations`, run `20261031-232010`, PASS. The captured
frame was inspected. The recipe verifies the real commit, file content,
approval event, and model-facing tool result. Evidence is under
`.verify/evidence/20261031-232010/developer-operations/`.

Push tests use a trusted host SSH helper connected to a temporary bare
repository. Issue and PR executor tests use recording executables. No live
GitHub write or Tier 2 real-model validation has been performed.

Tier 1 release comparisons use baseline `894f847` and this working tree:
[baseline size](../../bench/results/size-20261031-224557.json),
[size](../../bench/results/size-20261031-232054.json),
[frame](../../bench/results/frame-20261031-232059.json), and
[tools](../../bench/results/tools-20261031-232102.json).
`bench.py compare` reports frame and tool timings as `same`. The binary grows
from 14,890,320 to 15,006,176 bytes; the plain first request grows by 106 bytes.
The memory suite cannot run because the opencode corpus is absent; its RSS
sampler also uses Linux `/proc`.

Saving benchmark results makes the quick gate include benchmark fixture
tests. Those fail on this Mac at the existing GNU `sed -i` command in the
release-signing solution and intermittently during fixture cleanup. The
release-signing failure is reproduced in the unchanged baseline checkout.
Rust checks and the application tests above pass; the expanded local quick
gate is therefore not wholly green.
