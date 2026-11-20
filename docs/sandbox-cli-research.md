# Sandboxed shell and authenticated CLI access

Research date: 2026-10-31. This note records current behavior and recommendations, not a settled architecture decision.

## Construct today

`crates/construct/src/sandbox/mod.rs` permits executing programs in system and toolchain paths. Ordinary commands do not need command-name exceptions. Their filesystem access remains confined. On Linux the backend is Landlock; on macOS it is Seatbelt.

`Policy::new` creates `$CONSTRUCT_HOME/tmp/<pid>`, grants it write access, and `Policy::spawn` sets `TMPDIR` to it. Literal `/tmp` is outside this grant. `[sandbox] allow_read` and `allow_write` add explicit roots; roots must exist when the policy is built because nonexistent paths are filtered during canonicalization.

`tools/shell.rs` identifies named outside paths before execution and infers denied paths from failed command output afterwards. `permission/perms.rs::shell` asks for access and reruns with widened roots. This grants paths, not host credentials. `scrub_env` removes credential variables, including `GH_TOKEN` and `GITHUB_TOKEN`, and removes `SSH_AUTH_SOCK` even with sandbox mode off.

## Other harnesses

- Codex distinguishes OS sandbox policy from approval policy. Workspace-write permits the workspace and temporary directories such as `/tmp`; network access is off by default. On-request approvals cover operations beyond the boundary. [Official OpenAI documentation](https://learn.chatgpt.com/docs/agent-approvals-security).
- Claude Code's optional Bash sandbox permits writes in the working directory and a per-user temporary directory, sets `TMPDIR`, and supports explicit additional roots. Sandboxed commands can run without individual prompts. Commands can retry unsandboxed subject to permission rules; strict settings can prohibit retries. Its shell sandbox does not cover file tools, MCP servers, or hooks. [Official Claude Code documentation](https://code.claude.com/docs/en/sandboxing).

These are different default threat models. Construct already restricts reads under the user's home more tightly than Claude's documented default.

## Why gh needs separate treatment

GitHub CLI normally stores its token in the system credential store, falling back to a plaintext file when that store is unavailable. It also accepts an environment token for headless operation. Allowing `~/.config/gh` alone therefore does not guarantee authentication; keychain access or an explicit credential path may still be required. [GitHub CLI authentication manual](https://cli.github.com/manual/gh_auth_login).

A keychain lookup that fails inside a sandbox does not establish that the user has no credential outside it. Likewise, a private repository returning 404 anonymously does not establish that it is absent.

## Recommended direction

Keep ordinary shell commands sandboxed. Add toolchain paths when a legitimate executable needs them, rather than maintaining a list of supposedly safe commands: interpreters, git hooks, aliases, and subprocesses make command names an inadequate confinement boundary.

Keep the private temporary directory as the default and teach the agent to use `$TMPDIR`. Offer explicit additional roots for interoperability. Users can grant `/tmp`, but that exposes a shared directory rather than just construct's scratch files. For compatibility with programs that hardcode `/tmp`, a private mounted `/tmp` on Linux is a possible later design; it requires namespace support beyond Landlock alone.

Treat authenticated external actions as a separate approved capability. A host-side runner or GitHub integration can execute a concrete approved operation with host authentication while keeping tokens out of model-visible shell output. Bind approval to the executable, arguments, working directory, and approved input contents; do not pass an arbitrary shell fragment to a privileged runner. Limit initial support to operations such as issue creation with an explicit repository and reviewed body.

An alternative is explicit per-tool credential injection with a short-lived, narrowly scoped token. That preserves filesystem confinement but lets the executing program read the token, so it is a different security guarantee from a broker. Do not silently turn on all credential inheritance or make the entire keychain available to every shell command.
