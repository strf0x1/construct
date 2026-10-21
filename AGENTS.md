# Working on construct

construct is a single-binary agent harness in Rust: an OpenAI-compatible
streaming loop, in-process tools, capped memory and agent-written skills, a
background learning pass, a Jev/local router, secretspec secrets, and a
full-screen TUI. `construct-dev` (in `crates/construct-dev/`) is the tooling that lets you run
and observe it end to end without a model or a terminal.

## Layout

| Path | What |
|---|---|
| `crates/construct/src/cli.rs` | Clap types and subcommand bodies; `main.rs` is only the sandbox-helper exit and `cli::run()`. Logic lives in the lib. |
| `crates/construct/src/agent.rs` | The loop: route, stream, run tools, compact. Everything observable is an `Event`. |
| `crates/construct/src/llm.rs` | Streaming `/chat/completions` client (hand-rolled SSE). |
| `crates/construct/src/tools/` | One `impl Tool` per file (shell, read, write, edit, grep, glob, web, memory, skill, history); `edit/hashline/` is the anchored read/edit pair; `mod.rs` holds the trait and the `Catalog`, built from `[tools]` config. |
| `crates/construct/src/router.rs` | Per-turn tool groups + skill suggestions: Jev when `TYPESAFE_API_KEY` resolves, local otherwise. |
| `crates/construct/src/memory.rs`, `crates/construct/src/skills.rs` | Capped `§` memory (USER.md, MEMORY.md); skill library with usage stats, archive-only deletes. |
| `crates/construct/src/learn.rs` | Learning gate (Jev or local), observation ledger, recurrence-gated review, curator. Runs detached after a session. |
| `crates/construct/src/jev.rs` | Every Jev call: state budget, failure breaker, `jev.log`. |
| `crates/construct/src/sandbox/` | Tool boundary: Landlock for shell (via `construct __sandbox`), path checks for file tools, credential-free env. `supervise.rs`: that helper is a subreaper, and nothing a shell call starts outlives it. |
| `crates/construct/src/tui/` | ratatui app, markdown renderer, theme. |
| `crates/construct-dev/src/serve.rs` | Scripted or recording OpenAI-compatible endpoint. |
| `crates/construct-dev/src/drive.rs` | Runs any TUI in a pty, answers terminal queries, scripts keystrokes, snapshots the screen. |
| `crates/construct-dev/tests/` | End-to-end tests (`e2e.rs`) and golden snapshots (`snapshots.rs`): real binary, fresh home, scripted model. |
| `crates/construct/tests/` | Contract tests: link the lib and pin one module's promise (e.g. edit tiers). |
| `bench/` | Benchmark against Hermes Agent and opencode; results in `bench/results/`. |
| `bench/tasks/` | Real-model tasks as directories: `task.toml`, `fixture/`, `check.sh` (see `bench/tasklib.py`). |

## Run it without a model

```sh
cargo build -p construct -p construct-dev
# a scripted model: steps are text or tool calls (adapted to the harness's tool names)
echo '{"steps":[{"call":"read","args":{"path":"Cargo.toml"}},{"text":"done"}]}' > /tmp/s.json
port=$(scripts/mock.sh /tmp/mock /tmp/s.json)        # requests logged to /tmp/mock/log.jsonl
CONSTRUCT_HOME=/tmp/ch CONSTRUCT_BASE_URL=http://127.0.0.1:$port/v1 OPENAI_API_KEY=x \
  target/debug/construct run --json "read it"         # one JSON event per line
```

## Drive the TUI and look at it

```sh
target/debug/construct-dev drive --metrics --env CONSTRUCT_HOME=/tmp/ch \
  --env CONSTRUCT_BASE_URL=http://127.0.0.1:$port/v1 --env OPENAI_API_KEY=x \
  -- target/debug/construct <<'EOF'
wait "ask construct" 3000
type read it
key enter
wait "done" 5000
snap after-turn
EOF
```

Script commands: `wait`, `gone`, `type`, `key` (enter esc tab up down pgup pgdn
ctrl-x alt-enter …), `sleep`, `snap`, `expect`, `resize`, `settle`. `--metrics`
prints first-output, first-frame and per-wait timings as JSON.

## Tests

```sh
cargo xtask check                 # fmt, clippy -D warnings, all tests, bench tests; ends in `ok` or `FAILED <gate>`
cargo xtask fix                   # clippy --fix, then cargo fmt
cargo test -p construct           # unit and contract tests (edit tiers, router)
cargo test -p construct-dev       # end-to-end: headless, router, learning, MCP, TUI
cargo xtask snapshots             # re-record golden snapshots after an intended change
```

`cargo xtask check` must print `ok` before a PR is opened; CI runs the same
command. Timing limits in e2e tests are smoke limits, doubled under
`CONSTRUCT_E2E_SLOW=1` (set in CI). Formatting is `cargo fmt`'s opinion; run
`git config blame.ignoreRevsFile .git-blame-ignore-revs` to skip the
reformat commit in blame.

`crates/construct-dev/tests/snapshots.rs` pins what the model sees (the first request for a
plain turn, a routed skill, routed web, routed MCP, and the learning review,
with `bytes` and `n_tools`) and what the user sees (first frame, a finished
turn). A refactor must leave them unchanged. When a change is meant to alter
them, `cargo xtask check` prints `FAILED snapshots` with a diff: read it, run
`cargo xtask snapshots` to accept, and say so in the PR. Never edit a `.snap`
by hand.

Prefer adding an e2e scenario over unit tests: assert on what a user or the
model would observe (files, requests sent, screen text). Each test gets a fresh
`CONSTRUCT_HOME` and its own scripted endpoint; read requests back with
`Env::requests()`.

## Verification tiers

Every change needs tier 0. Anything in the right-hand column also needs the
tier named; post the number either way.

| Tier | Command | Deterministic | Cost | Required for |
|---|---|---|---|---|
| 0 check | `cargo xtask check` | yes | ~1 min warm, no network | every PR |
| 1 overhead | `bench.py size frame tools memory` (mock model) | timing noise only | ~5 min, no network | startup, the TUI loop, tool execution, request building |
| 2 behaviour | `bench.py task\|routing\|loop --model …` (real model) | no | minutes to an hour, costs money | prompts, tool schemas or wording, router, learning, editor |

Tier 1 and 2 results carry a manifest; judge them with `bench.py compare`
(below). A decision they settle gets an ADR in `docs/adr/` (see `docs/README.md`).

## Conventions

1. `main.rs` only parses arguments and hands off. Anything testable lives in
   the lib.
2. Every module starts with a `//!` paragraph: what it is for and the one
   design choice a reader must know.
3. A file is one concern. Soft ceiling 300 lines; `tui/view.rs` and
   `tools/mod.rs` may run longer. Split by concern, never by line count.
4. Errors: `anyhow::Result` at the binary and in glue; `Result<T, String>` at
   the tool boundary stays, because that string is prose for the model. No
   `unwrap()` outside tests except on a `Mutex` lock, with a comment.
5. Tool names, group labels and other wire strings are declared once and
   referenced, never retyped as literals in another module.
6. Tests: unit tests inline under `#[cfg(test)]` for pure functions; contract
   tests in `crates/construct/tests/` for a module's promise to the rest of the
   harness; e2e in `construct-dev` for what a user or model observes. Prefer
   the highest tier that can observe the behaviour.
7. `cargo xtask check` must pass before a PR is opened. A refactor PR must also
   leave the golden request snapshots unchanged.
8. Dependencies: add one only if it removes more code than it adds; pin in
   `[workspace.dependencies]`; no git deps.
9. Formatting is `cargo fmt`'s opinion, never hand-tuned.
10. A decision that constrains future work gets an ADR (`docs/adr/`); check
    there before changing a settled behaviour.

## Rules of thumb

- A tool's wire name appears only in its `impl Tool` (`Shell::NAME` elsewhere).
  Nothing outside `tools/` matches on tool names: the UI uses `Display` and
  `OutputKind`, policy uses `Group` and `readonly()`. A tool not in the
  `Catalog` does not exist; `[tools] disabled` removes one, `editor` swaps the
  read/edit family (`"replace"`, or `"hashline"` with knobs under
  `[tools.hashline]`; `CONSTRUCT_TOOLS_EDITOR` for one run). `construct
  manifest` prints what a build and config run.
- First frame must not wait on anything: no terminal queries before drawing,
  no secret or network work on the UI path. `CONSTRUCT_TRACE=1` writes startup
  milestones to `$CONSTRUCT_HOME/trace.log`.
- Draw only when something changed, always inside a synchronized update.
- Keep the first request small: no skill index in the system prompt, core tools
  only; everything else is routed per turn and stays on once enabled.
- Learning must earn its tokens: the gate runs before any LLM review.
- Router/gate changes: score them with `python3 bench/jev/eval.py` (and
  `--set holdout`) before and after. Phrase nouls strictly ("explicitly
  require ...; answer no if ..."); in a choice put the conservative answer
  first, since Jev favours the first option.
- Tools run sandboxed (`[sandbox] mode = "workspace"`): the working dir, a
  private TMPDIR, system paths and toolchains. If a real tool breaks inside
  it, add the toolchain path to `HOME_RO`/`HOME_RW` in crates/construct/src/sandbox/mod.rs and a
  step to the sandbox e2e test, rather than turning the sandbox off.
- Never point a harness at a real model without the sandbox: an agent with
  nothing to go on searches the disk, and once read the bench key files.
- Benchmarks: `python3 bench/bench.py {size,frame,tools,memory,task}`, then
  `python3 bench/report.py`. Never rebuild `target/release/construct` while a
  bench is running. Every result carries a `manifest` of what produced it.
  Judge a change with `bench.py compare before.json after.json` (a verdict
  per metric, exit 1 on any `worse`), never by eye, and record it with
  `bench.py note "…" result.json`. A new task is a new directory in
  `bench/tasks/` whose `check.sh` scores the work dir, never the transcript;
  `bench.py task --tasks <name>` runs it.
- A/B a construct setting with arms, not by hand: `bench.py task --arm
  replace:'editor="replace"' --arm hashline:'editor="hashline"' --no-learn
  --tasks edit-…` writes each arm's `[tools]` overlay into its own home,
  interleaves the arms, records each arm's manifest, and `bench.py compare
  result.json --a replace --b hashline` judges them. Edit outcomes (rejections
  by code, recoveries, re-reads) come from the `tool` records in the session
  log, never from prose.

## Benchmarking against Hermes and opencode

The competitor harnesses aren't vendored — `research/src/` is gitignored. Only
their CLIs are needed, and the bench finds them on `$PATH` (or via `mise
which`, or a `BENCH_HERMES` / `BENCH_OPENCODE` override), so no version is
pinned. To bootstrap from nothing:

```sh
mise use -g pipx:hermes-agent opencode   # the two competitor CLIs
sudo pacman -S bubblewrap                 # or: apt install bubblewrap
cargo build --release -p construct && cargo build -p construct-dev
python3 bench/bench.py task --harness construct hermes opencode --model <id>
```

`preflight()` stops the run up front with an install hint if a requested
harness or `bwrap` is missing. hermes and opencode are always launched inside
bwrap (confined like construct's own Landlock sandbox — see `Sandboxed` in
bench.py); construct is not, because it sandboxes itself. Real-model runs still
need the key files and the proxy — see the security notes above.
