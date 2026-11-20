# 0053 — TLS trust roots for static builds

Status: accepted. 2026-11-20.

## Context

The static musl builds (#289, #271) run on scratch and busybox images, where
there is no certificate store at all. `rustls-platform-verifier` loads the
platform store through `rustls-native-certs`; with no store and no extra
roots, client construction fails with `rustls::Error::General("No CA
certificates were loaded from the system")`, which reqwest surfaces as the
opaque `builder error`. Alpine ships a bundle and works; busybox and scratch
media do not (#293).

Three ways out were on the table:

1. **Document the bundle.** Zero bytes, but "drop and run" keeps an asterisk,
   and the failure is the opaque one.
2. **Bake Mozilla's roots in as *extra* roots.** reqwest already implements
   the merge: with roots on the builder it calls
   `Verifier::new_with_extra_roots` (`client.rs:767`), which loads native
   roots *and* the extras, and tolerates an empty native store when extras
   exist (`others.rs:94-103`). The `rustls-tls-webpki-roots` feature would
   replace platform verification entirely — ruled out, it breaks enterprise
   CAs and TLS-intercepting proxies.
3. **`CONSTRUCT_CA_FILE` / `[tls] ca_file`.** Covers private CAs without
   touching the system store.

## Decision

- **Part 1: fail loudly, everywhere.** `http::build()` walks the reqwest
  error chain and replaces the empty-store failure with:
  `no CA certificates found: set SSL_CERT_FILE=/path/to/ca-certificates.crt
  (or CONSTRUCT_CA_FILE), or install ca-certificates`. Every client
  construction site goes through `http::build()` (`#290` made it one module;
  this is one call).
- **Part 2: baked roots, static musl only.** `builder()` folds
  `webpki-root-certs`' Mozilla set in as extra roots when
  `cfg(target_env = "musl")`. These are the only artifacts advertised as
  drop-in on rescue media. A managed host's enterprise/interception CA still
  wins because the platform store is loaded too. The glibc and macOS builds
  carry no baked roots: they run where a trust store exists, and the bytes
  buy nothing there.
- **Part 3: `CONSTRUCT_CA_FILE` on every build.** A PEM bundle (or one DER
  certificate) added as extra roots, merged the same way. Environment only,
  not `[tls] ca_file`: clients are built before and outside config loading
  (`models`, the dev proxy, the learning gate), so a config key would need a
  global solely for this. `SSL_CERT_FILE`/`SSL_CERT_DIR` remain the
  platform-store knobs, and `rustls-native-certs` honors them.
- The busybox release smoke now mounts **no** bundle and fetches the real
  model catalog, which proves the baked roots on the shipped artifact.

## Consequences

- Static musl binaries gain 128,549 bytes of DER (120 roots), plus alignment
  and the small loader. Measured on this tree, `x86_64-unknown-linux-musl`
  cannot be built locally (no musl target/zigbuild in the dev container), so
  the binary delta below is a **gnu** build with the cfg forced on; the DER
  payload is the floor.
- Roots are fixed at build time: a root removed from Mozilla's set stays
  trusted in an old binary until rebuilt. A managed host still checks its
  system store first, and a user can point `CONSTRUCT_CA_FILE` at a newer
  bundle. Static binaries are rebuilt every release, so the window is one
  release.
- The glibc build on a minimal container with no `ca-certificates` still
  fails; the message now says how to fix it, and the musl artifact is the
  drop-in one.
- `CONSTRUCT_CA_FILE` is read on each `http::build()`; a bad path fails the
  command with the variable and path named, not a TLS handshake error.
- Revisit if `webpki-root-certs`' license or size becomes a problem, or if
  musl builds get a way to select roots at runtime.

## Evidence

- `crates/construct-dev/tests/tls.rs`: empty store → actionable message;
  `SSL_CERT_FILE` → success; native + extra → both trusted; extra-only →
  success; DER accepted; bad path named. Fixtures under
  `crates/construct-dev/tests/fixtures/tls/`.
- `cargo test -p construct-dev --test it tls::` (6 tests).
- `.github/workflows/release.yml`, `smoke-static`: busybox fetch with no
  mounted bundle.
- Size, `[profile.release]` (`opt-level = "z"`, fat LTO, `strip`,
  `panic = "abort"`), x86_64-unknown-linux-gnu, baked-roots cfg forced on for
  the second row because this checkout has no musl target:

| build | bytes | Δ |
|---|---|---|
| without baked roots | 14,197,000 | — |
| with baked roots | 14,331,400 | **+134,400 B (+0.95%)** |

  The 120 Mozilla roots are 128,549 bytes of DER; the rest is alignment and
  the loader. The musl artifact is the only shipped build that pays it.

