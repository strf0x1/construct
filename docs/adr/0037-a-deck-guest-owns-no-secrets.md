# 0037 — A Deck guest owns no secrets

Status: accepted. 2026-11-03. Implements #169. Implements the construct side
of Deck ADR 0004 §4 and §10. Its optional-secret consequence is superseded by
ADR 0039.

## Context

Deck runs construct in a libkrun guest with no keyring service. The guest
receives a non-empty placeholder for each secret construct requires
(`OPENAI_API_KEY=deck-placeholder`), so environment-first secretspec resolves
it and skips the keyring; the host gateway strips the guest header and injects
the real value. The guest's `~/.construct` is on a disposable, agent-writable
disk, so anything stored there would be neither secret nor host policy.

Two guest-side paths told the user otherwise: `construct secrets set` went to
the keyring and failed without one, and `construct secrets status` showed a
placeholder as a resolved secret (`✓`) without saying it was a placeholder.
An unreachable keyring also printed provider warnings into a console that has
no store and never will.

The guest must keep working when there is no marker. On a host, the deployed
behavior is environment first, then the OS keyring.

## Decision

**The signal is `DECK_MANAGED`.** A non-empty value other than `0` or `false`
means Deck owns this task's credentials. Deck sets it in the VM task's guest
environment (deck ADR 0004 §4); the local-process backend does not.

**A managed guest resolves from the environment only.** `secrets::get` checks
the environment and stops; it never builds secretspec and never consults or
writes the keyring. `secrets::env_key` is unchanged (it was already
environment-only). Consequently `web_search`, the Jev router and MCP secrets
find only environment values, which Deck either supplies as placeholders or
does not supply at all — the v1 guest routes in deck ADR 0004 §3.

**`secrets set` refuses before reading a value.** In a managed guest it exits
non-zero with one line: secrets are host-owned and injected by Deck at the
gateway; use Deck's host-side credential UI or command. There is no fallback
to a guest file, and the command does not prompt for a value it cannot store.
`secrets::set` guards this itself, so a library caller cannot bypass it.

**`secrets status` reports the source, never the value.** A host keeps the
old `✓ NAME` / `· NAME` output. A managed guest marks an environment value
`✓ NAME (placeholder from environment)` and an absent one `· NAME`; because
the keyring is never consulted there, no line claims a keyring store. Status
holds only the name and a `Source::{Placeholder, Environment, Keyring,
Missing}`, so no formatting path can print a value.

**A missing endpoint key tells the truth.** In a managed guest the error names
the absent secret and says Deck injects host credentials at the gateway; the
host message still names `construct secrets set`.

## Consequences

- Host behavior and output are unchanged, including the keyring provider
  warnings that a host may legitimately need. A managed guest produces neither
  the secretspec audit note nor keyring warnings.
- A guest that unsets `DECK_MANAGED` gets the host code paths back; that only
  changes what its own CLI prints and attempts. It cannot make a keyring
  appear, and nothing in Deck's boundary depends on the marker.
- The marker is not a boundary. Guest environment values remain readable by
  every guest process, and the host gateway ignores the guest's credentials
  whichever mode construct thinks it is in.
- A new known secret needs no change here: managed status classifies whatever
  the environment supplies as a placeholder. Deck must set the marker and the
  placeholders for its tasks to get this behavior.

Revisit if a guest gains a real credential store or if Deck needs per-secret
provenance beyond "placeholder from environment".

## Evidence

- Issue #169 (acceptance: managed `secrets set` fails without the keyring,
  status names placeholders, no keyring warning, host output unchanged).
- `crates/construct/src/secrets.rs` unit tests: the marker's falsey values,
  and the status line per source.
- `crates/construct-dev/tests/deck.rs`: managed status output, managed set
  failing with the host-owned line without writing the value under
  `CONSTRUCT_HOME`, and host status keeping `✓ OPENAI_API_KEY`.
- `crates/construct-dev/tests/acp.rs` still proves that a host's secretspec
  and keyring diagnostics reach stderr; the managed path never builds that
  spec.
