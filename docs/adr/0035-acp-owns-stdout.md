# 0035 — In ACP mode, stdout belongs to the protocol

Status: accepted. 2026-11-02. Implements #162. Extends ADR 0015 ("ACP is a
translation at the process edge").

## Context

`construct acp` speaks newline-delimited JSON-RPC on stdout. Any other byte on
that descriptor is a frame a strict client cannot parse, and a client that
does not filter (or a relay that merges descriptors) desynchronises.

Deck's VM spike saw secretspec's first-run audit note and a keyring warning
interleaved with the ACP stream (construct#162). The note and the warning are
written with `eprintln!` in secretspec 0.21.1, so construct's stdout was
already clean; the guest relay (`socat SYSTEM:…,stderr`) merged the two
descriptors. The issue's requirement is still the right contract: diagnostics
must stay off stdout, and construct must not depend on the caller filtering.
Auditing today's call sites is not enough — a `println!` added by a dependency
later would silently corrupt a frame.

## Decision

`acp::server::run` takes stdout away from the rest of the process:

1. It duplicates fd 1 into a private `OwnedFd` and hands that to the single
   writer thread, which is the only producer of protocol frames.
2. It points fd 1 at fd 2 (`libc::dup2`) before any session work starts and
   checks the return.

Everything else that writes to stdout — a `println!`, a library with logging
configured to stdout, a diagnostic from a spawned helper that inherits the
descriptor — therefore lands on stderr. If either step fails, fd 1 keeps its
old meaning and the writer falls back to it: the result is the pre-change
behaviour, a warning naming the error goes to stderr, and the protocol still
works.

This applies to both `construct acp` (user mode) and `construct __child --acp`
(child mode); both speak the same protocol and both keep stderr for
diagnostics.

## Consequences

- The stdout-only-JSON-RPC guarantee is structural: it holds for code that has
  not been written yet, not just for the call sites audited today.
- Whoever merges stderr into the ACP stream (the VM relay did) still sees
  diagnostics inline. That is a caller-side choice; construct's contract is
  that they are not on stdout. A client that wants a pure stream keeps stderr
  separate.
- Tools, MCP servers and delegated children that speak their own protocol do so
  on explicit pipes, not the inherited fd 1, so the redirect does not change
  their transport. A child that inherits stdout for diagnostics writes to
  stderr, which is what its parent collects.
- A diagnostic that users need while editing sees the same text on stderr;
  nothing about its content or ordering changes.
- Revisit if: ACP needs a second in-band channel for diagnostics (the protocol
  has `_meta` and notifications for that), or if a caller needs the agent's
  stdout for something other than the protocol.

## Evidence

- #162 (reproduction, acceptance); strf0x1/deck#29 and
  `docs/vm-spike.md` for the merged-descriptor observation.
- e2e `acp::acp_stdout_is_only_json_rpc_and_diagnostics_are_stderr` drives
  `initialize`, `session/new` and a scripted prompt with a fresh secretspec
  state directory (forcing the first-run audit note) and an unreachable D-Bus
  address (forcing the keyring provider failure on Linux). `Client::call`
  parses every stdout line and rejects a non-`jsonrpc` line; the test asserts
  both diagnostics are present in the captured stderr.