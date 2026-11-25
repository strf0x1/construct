# 0073 — Stamped peer binaries: `construct build`, a fail-closed trailer, a layer under the file

Status: accepted. 2026-11-25. Part of #273, #363. Builds on ADR 0050, 0064,
0069, 0070, 0072.

## Context

The peer story (ADR 0068–0072) still ends in a long command line on the
target: `construct peer --url … --hub-key … --id …`, plus whatever config the
box needs, plus a binary for its architecture. #363 asks for one file that is
dropped on a machine without a compiler and run. ADR 0050 already lets a
config be compiled in, but that needs a compiler and a build per hub; a
stamp must be added to an already-built binary, including a published
release for another target.

Constraints: no new crate (`sha2` is already a workspace dependency); no
secret or join token in the file; a stamped sandbox policy must never be
lost silently; `--version`, `config` and the other commands must keep
working on a stamped binary.

## Decision

- **The stamp is appended**: `[binary][payload][sha256 (32 B)][len (u32
  LE)]["CNSTPEER" (8 B)]` (`construct-core/src/config/stamp.rs`). The
  payload is TOML: `stamp = 1` (the format), `[peer]` (`url`, `hub_key`,
  `id`, optional `cwd`, the `sandbox` preset's name) and `[config]`, any
  `config.toml` keys. Unknown keys and other formats are refused. At most
  64 KiB.
- **Read once, from the running binary**: `current_exe()`, falling back to
  `argv[0]` where there is no `/proc`. Only the last 44 bytes and the payload
  are read.
- **Fail closed.** A trailer whose length is zero, over the limit or larger
  than the file, a payload that fails its SHA-256, does not parse or names
  another format, or a file that ends partway through the marker
  (`CNST`…`CNSTPEE`, a copy cut short) is an error from `Config::load`,
  before any command runs, naming the binary. Only a binary with no trailer
  runs unstamped. Silently falling back would drop a stamped `[sandbox]`.
- **`[config]` is a layer of its own, between baked and file**: baked <
  stamped < file < env < flags, merged as ADR 0050 merges (tables key by
  key, scalars and arrays whole), with `stamped` provenance in `construct
  config`, which also prints the stamped peer (id, URL, key, preset). The
  file still wins, so a dropped binary stays tunable in the field.
- **A stamped binary with no arguments at all is `construct peer`**, which
  takes `--url`, `--hub-key`, `--id` and `--cwd` from the stamp when they are
  not given. Any argument means the ordinary CLI.
- **`construct build` lives on the host** (the binary's `hub` feature;
  `construct-hub/src/build.rs`): `--name`, `--url`, the controller key from
  this home (`--hub-key` overrides), `--settings <toml>`, `--sandbox
  workspace|readonly-host|off`, `--cwd`, `--out`. `llm = "hub"` is stamped
  unless the settings say otherwise (ADR 0072), so a stamped peer carries
  no API key. `--invite` mints the join token through the running hub or
  into the registry offline and prints `CONSTRUCT_JOIN=<token>
  ./construct-<name>`; the token is never in the file.
- **Sources**: `--from <path>`; else this binary when the target is the
  host's (`CONSTRUCT_BUILD_TARGET`, set by core's build.rs) and `--release`
  is not given; else the published `construct-<target>-<profile>` of this
  version, with `bare` the one profile, downloaded with its `.sha256`, cached
  in `$CONSTRUCT_HOME/cache/release/<version>/` and checked on every use. A
  stamp on the source is replaced; a broken one is refused. The written file
  is read back through the same reader before `build` reports success.
- **`readonly-host`** is `mode = "read-only"` plus `allow_read` of `/etc`,
  `/var/log`, `/var/lib`, `/proc`, `/sys`, `/run`, `/usr`, `/opt`, `/srv`,
  `/boot` (missing paths drop out): a diagnostic peer reads the host's
  configuration and logs, and every write asks first (ADR 0016).
- **Releases publish the bare peer raw**: `release.yml` builds
  `--no-default-features --features peer` for both static musl targets and
  uploads `construct-<target>-bare` and its checksum next to the archives,
  so `build` needs no tar or gzip reader.

## Consequences

- Stamping costs nothing at run time for an unstamped binary beyond reading
  44 bytes of its own file once.
- A stamped macOS binary fails `codesign --strict`: appended bytes sit
  outside the signature. It runs (tested 2026-11-22 and in the e2e suite on
  macOS); a notarized variant would need a reserved section and is out of
  scope until notarized builds exist.
- A stamp is readable by anyone holding the file: the hub's URL and public
  key, the peer id, the policy. That is the intent; nothing in it grants
  access without a join token or an approval (ADR 0070).
- The trailer cannot catch a copy cut by more than the marker (the length
  and checksum are gone with it); such a file is a truncated executable and
  is unlikely to start at all.
- Installing a stamped binary as a service (`--install`) is a separate step
  of #273.

## Evidence

Unit tests in `config/stamp.rs` (round trip, every corruption, the file
reader) and `config/baked.rs` (the stamped layer's precedence); in
`construct-hub/src/build.rs` (the `llm` default, the presets, restamping,
checksum lines). e2e `crates/construct-dev/tests/stamp.rs`: a binary stamped
`--from` shows its settings with `stamped` provenance in a home with no
config file; run with no arguments it joins a real hub with the token
`build --invite` printed; a flipped payload byte, an impossible length and a
cut marker each exit non-zero naming the checksum or truncation; a
`readonly-host` peer reads `/var/log` and has its write refused.
