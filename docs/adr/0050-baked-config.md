# 0050 — The baked config layer

Status: accepted. 2026-11-19.

## Context

#271 asks for the easiest drop-and-run install: compile a construct
pre-configured for your router and tailnet, drop the binary on a device, run
it with no config file. The binary already resolves everything from one
`config.toml` under `$CONSTRUCT_HOME`, falling back to defaults when the file
is missing — so a compile-time layer under that file is the whole feature.
The size hillclimb on the same issue measured the levers (ring, opt-z, the
secretspec subtree); this ADR settles the config mechanism, not the size
work.

Options on the table: a separate baked struct with `Option` fields per key
(invasive, duplicates every default), env-var-per-key baking (no templates,
no nested tables), and a TOML layer merged under the file (one mechanism,
every existing and future key bakes for free).

## Decision

1. `CONSTRUCT_BAKE_CONFIG=<path>` at build time embeds that config.toml into
   `construct-core` (build.rs validates it as TOML and writes it to
   `OUT_DIR`; `config/baked.rs` includes it). Unset means an empty layer and
   an ordinary build. A bad bake fails the build, not the first run on the
   target machine.
2. Precedence is **baked < file < env < flags**. Layers merge at the TOML
   level: tables merge key by key to the leaves; scalars and arrays replace
   whole (a file's `[tools] disabled` overrides the baked list, it does not
   extend it). One deserialize of the merged table, so serde defaults apply
   exactly once.
3. **Bake policy, never secrets.** Endpoint, model, sandbox mode, tool
   toggles, skill-related settings bake. Keys resolve through secretspec at
   runtime (env first, then keyring; ADR 0039's guest pattern already shows
   the shape). build.rs warns on the one config corner where a literal
   credential can hide: auth-looking `[headers]` / `[providers.*.headers]`.
4. **Provenance is visible.** Every leaf key records its layer
   (`Config::provenance`, serde-skipped); `construct config` prints
   `# key = baked|file|env` under the dump. The env override table in
   `config/baked.rs` is the single source of truth for what env sets and what
   provenance reports. Invisible config in a dropped tool is a debugging
   nightmare.
5. A baked `context_window` pins the window exactly like a file one.

## Consequences

- A preconfigured build is `CONSTRUCT_BAKE_CONFIG=policy.toml cargo build
  --release -p construct` — no code fork, no second binary crate. Every
  config key added later bakes for free.
- The on-disk file always wins, so a dropped binary stays tunable in the
  field; env stays the one-run override for benches.
- The config hash in `manifest` is unchanged (provenance is serde-skipped),
  so bench comparability holds.
- Skill-pack embedding (compile a skills directory in, seed it on first run)
  is a follow-up on #271, not this layer; the sandbox policy template is
  already just `[sandbox]` in the bake file.
- Revisit if a bake ever needs to be *unoverridable* (a locked-down policy
  preset): that is a different mechanism (enforcement, not defaults) and
  would need its own decision.

## Evidence

#271 (design thread, rounds 1–4). Unit tests in `config/baked.rs` pin the
merge precedence; the e2e `config_dump_shows_where_each_setting_came_from`
pins the visible provenance. A baked build was driven end to end against a
scripted model (PR description).
