# Working on construct

construct is a single-binary agent harness in Rust: an OpenAI-compatible
streaming loop, in-process tools, capped memory and agent-written skills, a
background learning pass, a Jev/local router, secretspec secrets, and a
full-screen TUI. `construct-dev` (in `dev/`) is the tooling that lets you run
and observe it end to end without a model or a terminal.

## Layout

| Path | What |
|---|---|
| `src/agent.rs` | The loop: route, stream, run tools, compact. Everything observable is an `Event`. |
| `src/llm.rs` | Streaming `/chat/completions` client (hand-rolled SSE). |
| `src/tools/` | shell, read/write/edit/grep/glob (ripgrep crates), web, memory/skill/session_search dispatch. |
| `src/router.rs` | Per-turn tool groups + skill suggestions: Jev when `TYPESAFE_API_KEY` resolves, local otherwise. |
| `src/memory.rs`, `src/skills.rs` | Capped `§` memory (USER.md, MEMORY.md); skill library with usage stats, archive-only deletes. |
| `src/learn.rs` | Learning gate (Jev or local), observation ledger, recurrence-gated review, curator. Runs detached after a session. |
| `src/jev.rs` | Every Jev call: state budget, failure breaker, `jev.log`. |
| `src/sandbox.rs` | Tool boundary: Landlock for shell (via `construct __sandbox`), path checks for file tools, credential-free env. |
| `src/tui/` | ratatui app, markdown renderer, theme. |
| `dev/src/serve.rs` | Scripted or recording OpenAI-compatible endpoint. |
| `dev/src/drive.rs` | Runs any TUI in a pty, answers terminal queries, scripts keystrokes, snapshots the screen. |
| `dev/tests/e2e.rs` | End-to-end tests (real binary, fresh home, scripted model). |
| `bench/` | Benchmark against Hermes Agent and opencode; results in `bench/results/`. |

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
cargo test                        # unit tests (edit tiers, router)
cargo test -p construct-dev       # end-to-end: headless, router, learning, MCP, TUI
```

Prefer adding an e2e scenario over unit tests: assert on what a user or the
model would observe (files, requests sent, screen text). Each test gets a fresh
`CONSTRUCT_HOME` and its own scripted endpoint; read requests back with
`Env::requests()`.

## Rules of thumb

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
  it, add the toolchain path to `HOME_RO`/`HOME_RW` in src/sandbox.rs and a
  step to the sandbox e2e test, rather than turning the sandbox off.
- Never point a harness at a real model without the sandbox: an agent with
  nothing to go on searches the disk, and once read the bench key files.
- Benchmarks: `python3 bench/bench.py {size,frame,tools,memory,task}`, then
  `python3 bench/report.py`. Never rebuild `target/release/construct` while a
  bench is running.
