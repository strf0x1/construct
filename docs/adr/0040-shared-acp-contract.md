# 0040 — The Deck-Construct ACP extensions are a versioned shared contract

Status: accepted. 2026-11-03. Follow-up to 0038, construct#171 and deck#36.

## Context

ADR 0038 renamed the ACP extensions under `com.sprawlcommons` and promised one
release of dual names. Both implementations then carried their own prose
(Construct's ADR 0038, Deck's ADR 0005) and their own e2e tests for the same
wire. A change lands in one repo first, so the pair was kept in agreement by
reading both ADR sets and matching tests; nothing failed when they drifted.
Renames are exactly the change that needs one owner.

## Decision

- The contract lives in `strf0x1/sprawlcommons-acp` (private, semver, tagged
  `v1.0.0`). It contains the normative spec, JSON Schemas, the negotiation
  matrix and the change protocol; it contains no implementation.
- Construct pins a release in `CONTRACT.lock`: repository, version, commit,
  vendored directory and the sha256 of the vendored tree.
- Only the machine artifacts are vendored
  (`crates/construct/tests/fixtures/acp-contract/<version>/`: `VERSION`,
  `matrix.json`, `schema/`). The prose stays in the contract repository and is
  cited by version.
- `crates/construct/tests/acp_contract.rs` asserts `acp/map.rs`'s constants and
  `Wire::for_client` against the vendored matrix.
- `scripts/acp-contract.sh verify` hashes the vendored tree against the lock;
  the `contract` gate in `cargo xtask check` runs it. `update <version>`
  downloads a release with `gh` and rewrites the lock.
- A wire change starts in the contract repo (semver; matrix and schemas in the
  same PR), then lands in Construct and Deck pinned in the same wave. An
  implementation-only wire change is not accepted. Removing the bare names is
  a contract major (2.0.0), per 0038.

## Consequences

- One place answers what a name or payload is; a construct agent and a deck
  agent read the same matrix instead of each other's ADRs.
- Construct's public tree gains only names and payload shapes already public
  in 0038 and its tests; the normative prose and compatibility policy remain
  in the private contract repository.
- A vendored copy can be stale. The hash gate fails it, and a version bump is
  a visible diff rather than a silent one.
- Updating the pin needs `gh` locally; verification is offline and runs in CI.
- Two extension names live in the contract, not in either implementation's
  vocabulary. A new extension starts as a contract minor, before any code.

## Evidence

- `strf0x1/sprawlcommons-acp` v1.0.0, commit
  `8c24a07c28aaabd539582d749a5ee9c583ca107d`, extracted from 0038 and deck
  ADR 0005 without a behavior change.
- `CONTRACT.lock`, `crates/construct/tests/acp_contract.rs`,
  `scripts/acp-contract.sh verify`, the `contract` gate in
  `crates/construct-dev/src/xtask.rs`.
- Construct#171 and deck#36 (compatibility matrix); PR #176 pins the first
  release.

## Payload conformance (contract 1.0.1 candidate)

The shared payload fixtures cover valid and invalid forms for all seven
schemas. Tests use a standard JSON Schema validator with network resolution
disabled and every referenced schema registered from the vendored tree.
Actual ACP payloads are also checked, so agreement on names alone is not
sufficient. The validator is a dev dependency only, absent from shipped
binaries. Contract 1.0.1 corrects schemas to match existing wire rules; it
changes no negotiation or method names. Candidate pins stay in draft until
the release tag exists and both implementations adopt it.
