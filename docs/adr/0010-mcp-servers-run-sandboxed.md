# 0010 — MCP servers run sandboxed, like shell commands

Status: accepted, 2026-10-21. Implements #3. Builds on 0004 (Landlock via
re-exec) and 0006 (the helper supervises what it runs).

## Context

Every built-in tool runs inside the sandbox, but `[mcp.*]` servers were
spawned directly. They got construct's full environment (every credential)
and no filesystem boundary, so a model-chosen MCP call could read anything on
the disk. Nothing contained their lifetime either: `npx` starts `node`, and
`kill_on_drop` killed only `npx`.

The options were a separate mechanism for servers or the one shell already
uses. #32 (delegated children) will need the same per-process policy, and
#3 noted that whichever landed first should define it.

## Decision

- **Same helper, same policy.** A server starts as `construct __sandbox
  <policy> -- <command> <args>` through `Policy::spawn`, which the shell tool
  now uses too. The policy is `Policy::for_mcp`: the `[sandbox]` policy (mode,
  workspace, private TMPDIR, system paths, toolchains), plus the entry's
  `allow_read` and `allow_write`, plus read and exec on the configured
  program itself, found on `PATH` the way the shell would find it. Mode
  `"off"` removes the filesystem boundary for servers as it does for shell,
  and keeps the helper.
- **Environment.** Servers get `scrub_env` like shell: no credential-looking
  variable and none of construct's own secrets. Only the names listed in the
  entry's `secrets` are added back, resolved like construct's own (the
  environment, then the keyring). A named secret that isn't set is left out,
  because many keys are optional. `env` still sets literal values.
- **Network** is on by default, since most servers are API clients.
  `network = false` makes Landlock handle TCP bind and connect with no rule
  allowing any port, which denies both (ABI 4, Linux 6.7+; best effort on
  older kernels, like the rest of the ruleset). UDP, DNS included, is not
  covered.
- **Protocol stream.** The helper merges a shell's stderr into stdout. For a
  server it leaves stderr where the helper's goes (null), because stdout is
  JSON-RPC and servers log to stderr. The policy field is `own_stderr`.
- **Lifetime.** Dropping a connection sends the helper SIGTERM, not SIGKILL,
  so it sweeps the server's whole tree. If construct dies, the helper's
  parent-death signal does the same (0006).
- **Visibility.** `construct config` ends with each server's effective read
  and write roots, network, and whether each named secret is set.

Example, left out of the default config:

```toml
[mcp.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]
description = "Up-to-date library and framework documentation and code examples"
secrets = ["CONTEXT7_API_KEY"]   # optional; higher rate limits when set
```

This works under the default policy with no `allow_*`: node comes from the
toolchain paths, and the npx cache is in `~/.npm`, which `HOME_RW` already
allows.

## Consequences

- A server that reads outside the workspace has to say so in its entry, and
  until it does it fails where it would have read. `construct config` shows
  what it got.
- A server binary outside system paths can run, but its own data directory
  has to be listed in `allow_read`.
- #32 can reuse `Policy::spawn` and the named-secrets rule for children.
- Revisit if: servers need per-host network rules (Landlock filters ports,
  not hosts), or a server needs something the helper's supervision breaks.

## Evidence

- e2e (`crates/construct-dev/tests/e2e.rs`, "mcp sandbox" section, using
  `construct-dev mcp-fake` and its unlisted probe tools, so the routed MCP
  snapshot doesn't change):
  - A read outside the workspace is denied, while a read in the workspace and
    under the entry's `allow_read` succeeds.
  - The server's environment has the named secret and none of an unnamed
    `*_TOKEN`, `OPENAI_API_KEY` or an unset named secret.
  - `network = false` denies a TCP connect to a local listener, and the same
    connect succeeds for a server with the default.
  - A `setsid sleep 300` started by a server is gone once construct exits.
- `context7_answers_from_inside_the_sandbox` (`#[ignore]`; needs node and the
  internet): `resolve-library-id` for rusqlite returns `/rusqlite/rusqlite`
  through the sandboxed server. It passed on the machine that made this
  change, with no Context7 processes left afterwards.
- Golden snapshots unchanged: `routed_mcp` still sends the same request.
