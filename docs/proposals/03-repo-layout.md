# Proposal 3 — A model Rust layout for construct

Status: plan, not started. Written 2026-10-20 against `1e83f33`.

The code is in good shape for a six-hour build: every module has a `//!` header
that says what it is for, the e2e suite runs the real binary against a scripted
model in ~11s, and the loop/tools/router/learn split is right. What it lacks is
the *scaffolding* a Rust project is expected to have, so that an agent dropping
in knows where things go and a checker tells it when it strayed. That is what
this plan adds. It changes no behaviour.

## Where it stands today

| Item | Now | Model Rust repo |
|---|---|---|
| Crate shape | `construct` is a binary-only package; every module is `mod x;` in `main.rs` | thin `main.rs`, everything in `lib.rs`, so tests and tooling can link it |
| Workspace | root package + `dev/` member | virtual root, members under `crates/`, shared `[workspace.package]` and `[workspace.dependencies]` |
| Formatting | no `rustfmt.toml`; 388 hunks differ from default rustfmt, 426 lines over 100 cols | one committed config, `cargo fmt --check` in CI |
| Lints | 39 clippy warnings in `construct`; one *denied* lint in `construct-dev` (`never_loop`, dev/src/drive.rs:101) so `cargo clippy --workspace` fails | `[workspace.lints]` set once, `-D warnings` in CI |
| CI | none | fmt, clippy, test on every PR |
| License | `license = "MIT"` in Cargo.toml, no `LICENSE` file | file present |
| Big files | `src/tui/mod.rs` 1158 lines, `src/learn.rs` 638, `src/tools/mod.rs` 422 | one concern per file, ~300 lines as a soft ceiling |
| Tests | 4 unit tests, 10 e2e; no library-level tests possible (no lib) | unit (inline), contract (`crates/construct/tests/`), e2e (`construct-dev/tests/`) |
| Docs | README (why), AGENTS.md (how), `docs/proposals/` | plus `docs/adr/` for decisions that were taken, and a `docs/README.md` index |
| Toolchain | `rust-version = "1.90"`, edition 2024 | plus `rust-toolchain.toml` so every agent builds with the same compiler |

`unsafe` appears in two files (shell process-group handling, TUI). That is fine
and expected for a sandboxing harness; the plan asks for a `// SAFETY:` comment
on each, nothing more.

## Target layout

```
construct/
├── Cargo.toml                 virtual workspace: members, workspace.package, workspace.dependencies, workspace.lints
├── Cargo.lock
├── rust-toolchain.toml        channel = "stable" (pin a version if a bench needs reproducibility)
├── rustfmt.toml
├── .editorconfig
├── LICENSE                    MIT text
├── README.md                  the why (unchanged)
├── AGENTS.md                  the how: build, run, test, verify, conventions (paths updated)
├── .github/workflows/ci.yml
├── crates/
│   ├── construct/             the product
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── main.rs        parse CLI, build runtime, call into the lib. ~60 lines.
│   │   │   ├── lib.rs         `pub mod` list + crate docs. No logic.
│   │   │   ├── cli.rs         clap types and the subcommand bodies (today's main.rs minus `main()`)
│   │   │   ├── agent/         mod.rs (Agent, turn loop), event.rs (Event), compact.rs
│   │   │   ├── llm/           mod.rs (types), client.rs (SSE streaming)
│   │   │   ├── tools/         see proposal 04: mod.rs (Tool trait, Catalog), one file per tool
│   │   │   ├── router/        mod.rs (route, Input, Route), jev.rs, local.rs
│   │   │   ├── learn/         mod.rs (entry points), gate.rs, review.rs, ledger.rs, curator.rs
│   │   │   ├── memory.rs, skills.rs, project.rs, session.rs, prompt.rs
│   │   │   ├── config.rs, secrets.rs, sandbox.rs, mcp.rs
│   │   │   └── tui/           mod.rs (run), app.rs (state + update), view.rs (draw), input.rs (keys), markdown.rs, theme.rs
│   │   └── tests/             contract tests that link the lib: edit tiers, sandbox path checks, memory caps
│   └── construct-dev/         was dev/. Unchanged inside. Never depends on the construct lib:
│       ├── Cargo.toml         it observes the binary the way a user would.
│       ├── src/{main,lib,serve,drive}.rs
│       └── tests/e2e.rs
├── bench/                     unchanged (python; target/ paths are workspace-relative so nothing moves)
├── docs/
│   ├── README.md              index: what lives where, how proposals become ADRs
│   ├── adr/                   0001-record-decisions.md, template.md, then one file per accepted decision
│   ├── proposals/             as now
│   └── branding/
└── scripts/                   mock.sh, kill-matching.sh (paths updated)
```

Two decisions worth stating so nobody "improves" them:

- **One library crate, not five.** Splitting into `construct-core`, `-tools`,
  `-tui` buys parallel compiles and hard boundaries at the price of ceremony on
  every change. At 8k lines the module system plus `pub(crate)` is enough.
  Split a crate out only when a second binary needs it or a clean build passes
  ~90s.
- **`construct-dev` stays independent of the `construct` lib.** Its value is
  that it sees only what a user sees: a binary, a pty, HTTP requests. The
  moment it links the lib, e2e tests start asserting on internals.

## Conventions to write into AGENTS.md

Keep these short and absolute; agents follow rules better than guidance.

1. `main.rs` only parses arguments and hands off. Anything testable lives in
   the lib.
2. Every module starts with a `//!` paragraph: what it is for and the one
   design choice a reader must know. (Already true; keep it true.)
3. A file is one concern. Soft ceiling 300 lines; `tui/view.rs` and
   `tools/mod.rs` may run longer. Split by concern, never by line count.
4. Errors: `anyhow::Result` at the binary and in glue; `Result<T, String>` at
   the tool boundary stays, because that string is prose for the model (see
   proposal 04 for the structured form). No `unwrap()` outside tests except on
   a `Mutex` lock, with a comment.
5. Tool names, group labels and other wire strings are declared once and
   referenced (`Tool::name()`, `Group::label()`), never retyped as literals in
   another module.
6. Tests: unit tests inline under `#[cfg(test)]` for pure functions; contract
   tests in `crates/construct/tests/` for a module's promise to the rest of the
   harness; e2e in `construct-dev` for what a user or model observes. Prefer
   the highest tier that can observe the behaviour.
7. `cargo xtask check` (below) must pass before a PR is opened. A refactor PR
   must also leave the golden request snapshots unchanged (proposal 05).
8. Dependencies: add one only if it removes more code than it adds; pin in
   `[workspace.dependencies]`; no git deps.
9. Formatting is `cargo fmt`'s opinion, never hand-tuned.

## rustfmt and lints

`rustfmt.toml` (stable options only):

```toml
max_width = 120
use_small_heuristics = "Max"
newline_style = "Unix"
```

The code was written dense (struct literals and short matches on one line), and
`Max` heuristics keep that where it fits in 120 columns; default settings would
explode every `json!` block and tool definition. Expect the one-shot `cargo fmt`
to touch most files; do it in its own commit so blame survives
(`git blame --ignore-rev`, listed in `.git-blame-ignore-revs`).

Workspace lints in the root `Cargo.toml`:

```toml
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "warn"
missing_docs = "allow"          # module docs are required by convention; item docs are not

[workspace.lints.clippy]
all = "warn"
pedantic = "allow"              # turn on individual pedantic lints later if they earn it
dbg_macro = "warn"
todo = "warn"
unwrap_used = "allow"           # see convention 4; revisit once tools return structured errors
```

Each crate adds `[lints] workspace = true`. CI runs `cargo clippy --workspace
--all-targets -- -D warnings`.

## CI (`.github/workflows/ci.yml`)

One job, Linux only (Landlock is Linux-only and the sandbox e2e test needs it):

```yaml
name: ci
on: { push: { branches: [main] }, pull_request: {} }
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt, clippy }
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
```

The e2e suite asserts a first frame under 500ms; on a shared runner that will
flake. Make timing assertions read a threshold from `CONSTRUCT_E2E_SLOW=1`
(doubles the budgets) and set it in CI. Timing *measurements* belong to the
bench, not to pass/fail tests.

## The one command: `cargo xtask`

The Rust-idiomatic replacement for a Makefile is an `xtask` binary in the
workspace, run through a cargo alias. `construct-dev` already is that binary in
all but name; give it the subcommands and the alias rather than adding a crate:

```toml
# .cargo/config.toml
[alias]
xtask = "run -q -p construct-dev --"
```

```
cargo xtask check          fmt --check, clippy -D warnings, test --workspace. Exit code is the verdict.
cargo xtask fix            cargo fmt, clippy --fix
cargo xtask snapshots      re-record golden requests/screens after an intended change (proposal 05)
```

`check` prints one line per gate and a final `ok` / `FAILED <gate>`; agents
should never have to interpret cargo's output to know whether they are done.

## Phases

Each phase is one PR, lands green, and changes no behaviour. The e2e suite is
the behaviour oracle throughout. "Done when" is written so a model can check
itself.

### Phase 0 — guardrails (no file moves)

1. Add `rustfmt.toml`, run `cargo fmt --all`, commit as "style: apply rustfmt"
   alone. Add the commit sha to `.git-blame-ignore-revs`.
2. Fix the denied lint in dev/src/drive.rs:101 (`loop` that never loops: it is
   a `match` with early returns; rewrite as a plain block or make it loop as
   intended) and the 39 warnings in `construct`. Most are `collapsible_if` and
   `needless_borrow`; `cargo clippy --fix` handles 32 of them.
3. Add `[workspace.lints]` and `[lints] workspace = true` in both crates.
4. Add `LICENSE`, `rust-toolchain.toml`, `.editorconfig`, `.git-blame-ignore-revs`.
5. Add `.github/workflows/ci.yml` and the `CONSTRUCT_E2E_SLOW` budget switch.
6. Add the `xtask` alias and `construct-dev check`.

Done when: `cargo xtask check` is green locally and in CI on a PR.

### Phase 1 — lib/bin split, in place

1. Create `src/lib.rs` containing `pub mod agent; pub mod config; …` (every
   `mod` line from `main.rs`) and the crate doc comment (first paragraph of the
   README's "shape of it").
2. Move everything in `main.rs` except `fn main()` to `src/cli.rs`
   (`Cli`, `Cmd`, `headless`, `learn_cmd`, `route_cmd`, `gate_cmd`,
   `resolve_resume`). `main.rs` becomes: sandbox-helper early exit, trace,
   `cli::run()`.
3. Visibility: change `mod` to `pub mod` in `lib.rs`; inside modules make the
   items the CLI uses `pub`. Do **not** make everything `pub`; the compiler
   says what the bin needs.
4. Move `tools::edit::tests::tiers` out to `tests/edit_contract.rs` as the
   first contract test, so the tier exists.

Done when: `cargo xtask check` green; `target/debug/construct --help` output
is byte-identical to before.

### Phase 2 — `crates/`

1. `git mv src crates/construct/src`, `git mv Cargo.toml crates/construct/Cargo.toml`
   (then strip workspace keys from it), `git mv dev crates/construct-dev`.
2. New root `Cargo.toml`: `[workspace] resolver = "3" members = ["crates/*"]`,
   `[workspace.package]` (version, edition, rust-version, license, repository),
   `[workspace.dependencies]` for deps both crates use (anyhow, clap, futures,
   reqwest, serde, serde_json, tokio), `[workspace.lints]`, `[profile.release]`.
3. Update paths: AGENTS.md, `scripts/mock.sh` (uses `target/debug/…`, no
   change), `bench/bench.py` `ROOT`-relative target paths (no change), the e2e
   `root()` helper (`CARGO_MANIFEST_DIR` is now two levels down: use
   `env!("CARGO_WORKSPACE_DIR")` is not a thing; walk up until `Cargo.toml` with
   `[workspace]`, or read `cargo metadata --format-version 1 | .workspace_root`).

Done when: `cargo xtask check` green from a clean clone;
`python3 bench/bench.py tools --rounds 1 --harness construct` runs.

### Phase 3 — split the big files

Mechanical, one file per PR, zero logic changes. Order by pain:

1. `tui/mod.rs` → `tui/{mod,app,view,input}.rs`. `mod.rs` keeps `run()` and
   `trace()`; `app.rs` the `App` struct and its `update`/`on_event`; `view.rs`
   every `fn draw_*` and the layout; `input.rs` key handling and the input
   line editor.
2. `learn.rs` → `learn/{mod,gate,review,ledger,curator}.rs`. The prompts
   (`SYSTEM`, `MEMORY_FOCUS`, `SKILL_FOCUS`, `CURATOR`) go in `learn/prompts.rs`
   so a prompt change is a one-file diff a reviewer can read.
3. `tools/mod.rs` → per proposal 04 (one file per tool; `mod.rs` holds the
   trait and the catalog). This is the only phase-3 split that is also a
   design change; do it last, under proposal 04.
4. `agent.rs` → `agent/{mod,event,compact}.rs` (small; optional).

Done when (each): e2e green, golden request snapshots unchanged, `git diff
--stat` shows only moves plus `use` lines.

### Phase 4 — docs

1. `docs/README.md`: one paragraph per directory; the rule that a proposal
   becomes an ADR when it lands, and the ADR links the PR and the bench result.
2. `docs/adr/0001-record-decisions.md` and `docs/adr/template.md`
   (Context / Decision / Consequences / Evidence). Backfill three ADRs that
   are already decided and documented in code comments: strict nouls over
   choices for Jev routing (router.rs header + observations.md), Landlock
   sandbox via re-exec (`__sandbox`), groups that only grow within a session
   (tools/mod.rs header).
3. AGENTS.md: update paths, add the conventions list above, add the
   verification tiers table from proposal 05.

## Out of scope here

Behaviour changes of any kind, the tool catalog (proposal 04), bench changes
(proposal 05), and publishing to crates.io (not a goal; `publish = false` on
both crates).
