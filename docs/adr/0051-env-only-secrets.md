# 0051 — Env-only secrets: the keyring is a feature

Status: accepted. 2026-11-19.

## Context

#271's drop-and-run build bakes policy (ADR 0050) and must carry no
credential. The runtime side still pulled the whole secretspec subtree —
measured at ≈873 KiB at opt-z (secretspec + pgp + k256/bignum + miette/jiff,
plus zbus/secret-service on Linux), 20.2% of IR lines, heavier than the
entire TLS stack. A preconfigured binary dropped on a strange machine gets
its one endpoint key from the environment or a bootstrap flow; a keyring
(and an OpenPGP stack) is dead weight there, and on rescue media the D-Bus
secret service does not exist anyway.

The coupling made the gate cheap: secretspec is confined to `secrets.rs`,
and every consumer goes through a small facade (`get`, `optional`,
`managed`, `set`, `status`). The env-first path already exists, and
`DECK_MANAGED` guests already stop at the environment (ADR 0039).

## Decision

1. `construct-core` gains a `keyring` feature, **on by default**, gating the
   secretspec dependency (`keyring = ["dep:secretspec"]`). The `construct`
   binary forwards it (`default = ["keyring"]`); `construct-tui` takes the
   engine with `default-features = false` so it never forces the keyring on.
   `cargo build -p construct --no-default-features` is the env-only drop
   build; every shipped release build keeps the keyring.
2. The facade is unchanged. Without the feature, `keyring()` is a stub
   returning `None`, so `get`/`optional`/`source` keep their exact shape:
   environment first, Deck guests stop there, nothing else resolves.
   `set` refuses with `ENV_ONLY` ("this build has no keyring (env-only
   secrets); export the value in the environment instead"), and
   `missing_key` drops its `construct secrets set` suggestion.
3. `Source::Keyring` stays in the enum in both builds — it simply never
   occurs without the feature. No downstream `cfg`s.
4. A dedicated CI job (`check-env-only`) clippy-checks and unit-tests the
   engine without the feature, builds the binary, and smokes that env
   resolution works and `secrets set` refuses — so the gated path cannot
   rot while the workspace gates all build with defaults.

## Consequences

- The drop build drops the measured secretspec subtree; combined with the
  baked config layer it is the ~7 MB floor from the #271 attribution, with
  TLS + HTTP + async intact. Measure the exact combination shipped; lever
  savings are not additive across opt levels.
- One mechanism, two products: the same code is the host harness (keyring)
  and the dropped tool (env-only). No behavior fork beyond the stub.
- A bootstrap credential flow (single-use token → scoped session key in
  memory) lands on top of the env-only resolver later; this ADR does not
  design it.
- `construct-dev` keeps its own secretspec dependency for the recording
  proxy's `--key-secret`; the dev tool is not shipped and not gated.
- Revisit if a drop build ever needs a persistent secret store on the
  target: that is a different mechanism (e.g. an encrypted file unlocked
  by the bootstrap token), not the OS keyring.

## Evidence

#271 rounds 2–3 (cargo-bloat / cargo-llvm-lines attribution). Unit tests
in `secrets.rs` pin both feature states; the `check-env-only` CI job pins
the binary. Size deltas measured on the current tree in the PR.
