# construct

A fast, self-improving coding agent in a single Rust binary.

construct works with any OpenAI-compatible endpoint, runs its tools in a
sandbox, keeps a full record of every session, and learns from what goes
wrong. When a problem comes up more than once, it writes itself a skill or a
memory so it doesn't happen again.

## Install

Linux x86-64 (glibc 2.35 or newer), or macOS on Apple silicon or Intel:

```sh
curl -fsSL https://construct.sprawlcommons.com/install.sh | sh
```

The installer checks the release's SHA-256, makes sure the binary runs, and
puts `construct` in `~/.local/bin`. It doesn't use sudo or edit your shell
profile. Pass `sh -s -- --version vX.Y.Z` to pin a version, or download an
archive from [Releases](https://github.com/strf0x1/construct/releases).

The sandbox is Landlock on Linux and a Seatbelt profile on macOS. The macOS
build is new and has had less testing; what it does not do yet is in
[ADR 0028](docs/adr/0028-seatbelt-sandbox-on-macos.md).

To build from source:

```sh
cargo build --release -p construct   # target/release/construct
```

## Quick start

Store a key once. `construct secrets set` reads the key from stdin and keeps
it in the OS keyring:

```sh
construct secrets set OPENROUTER_API_KEY   # OpenRouter (the default endpoint)
construct secrets set OPENAI_API_KEY       # any other OpenAI-compatible endpoint
```

Then:

```sh
construct                                   # full-screen TUI in the current directory
construct run "fix the failing test"        # one prompt, headless
construct -c                                # continue the last session here
```

**OpenRouter** works out of the box and gives you the full model browser:
`/models` shows each model's context window and price.

**Local and other OpenAI-compatible servers** (llama.cpp, vLLM, Ollama, LM
Studio, OpenAI itself) are added as named providers. Each has its own url,
key and models, and you can switch between them like any model:

```sh
construct providers add local --url http://127.0.0.1:8080/v1
construct providers test local        # its model list, a short reply, a tool call
construct --model local/qwen3-coder   # or /model local/qwen3-coder in the TUI
```

`add` probes the server, works out how to read its models and context
windows (llama.cpp, vLLM, Ollama or a plain `/models`), and writes a table
into `~/.construct/config.toml`. You can also write it by hand:

```toml
model = "local/qwen3-coder"

[providers.local]                    # a server on this machine needs no key
base_url = "http://127.0.0.1:8080/v1"

[providers.gpubox]                   # another machine: its key is GPUBOX_API_KEY
base_url = "http://10.0.0.5:8000/v1"
discover = "vllm"                    # openai (the default), vllm, llama.cpp, ollama, off
context_window = 32768               # optional: the server's own figure is used otherwise
```

A model is named `provider/id`. A bare id works when only one provider's
catalog has it. Each provider's key goes to that provider only; store it
with `construct secrets set GPUBOX_API_KEY`. `construct providers` lists
them all, and `/model` groups models by provider and marks one that didn't
answer at startup. `[providers.openrouter]` on its own adds OpenRouter as a
named provider too.

For a single endpoint you can still set `base_url` and `model` at the top
of `config.toml` instead. That endpoint's key is `OPENAI_API_KEY`, or set
`api_key_secret = ""` for none. `CONSTRUCT_BASE_URL` and `CONSTRUCT_MODEL`
override them for one run.

Keys come from the environment first, then the OS keyring. `construct config`
shows the config path and the settings in effect.

Type `/` in the TUI to list commands (`/model`, `/skills`, `/memory`,
`/status`, …).

## Features

**Journal.** Each project's sessions are written to a SQLite journal
(`~/.construct/projects/<id>/journal.sqlite`), one row per event: messages,
tool calls, permission decisions, jobs. Resume uses it, `construct sessions`
reads and exports it, and the agent can query its own history with read-only
SQL.

**Subagents.** Define agents under `[agents.<name>]` and the model can
`delegate` a self-contained task to one. Each child is its own sandboxed
process with its own tools, model and prompt. It can also be an external ACP
agent, such as a Codex, Claude, Gemini or opencode adapter. When a child
needs more access, you get the same permission prompt, labeled with the
child's name.

**Background jobs.** A shell command or a delegated agent can run in the
background. The model gets a job id right away and keeps working. When the
job finishes, the agent picks it up on its own, whether that's a CI watch, a
slow test suite or a long-running child.

**Learning.** After a session ends, a background pass looks at what happened.
A cheap gate runs first and decides whether there's anything worth reviewing,
so most sessions cost nothing extra. What it learns becomes capped memory
(`USER.md`, `MEMORY.md`), per-project facts and conventions, or skills that
the agent writes and maintains itself. Anything it keeps is checked first for
credentials and for text that tries to steer the agent.

**Sandbox and permissions.** Shell commands and MCP servers run under
Landlock (Linux) or Seatbelt (macOS), file tools check paths, and the environment carries no
credentials. By default the agent can touch the working directory, a private
temp dir, and system paths and toolchains. Anything else asks you first; you
can allow it once, for the session, or permanently in config.

**Providers.** Several endpoints side by side, each with its own url, key
and models, selected as `provider/model` anywhere a model is accepted
(`--model`, `/model`, compaction, learning, subagents). Context windows come
from the server where it states them, and `/status` says which source set
the window. The design is in [ADR 0027](docs/adr/0027-named-providers.md).

**Compaction.** When history grows past the context window, construct stubs
old tool results first and only then summarizes older rounds. It recovers
from provider overflow errors, and a resumed session replays exactly the
same cuts.

**Routing.** The first request stays small, with just the core tools.
Additional tool groups (web, MCP, history) and skill suggestions are added
per turn by a router: Jev when a `TYPESAFE_API_KEY` is set, a local one
otherwise.

**Tools.** shell, read, write, edit (search/replace or hash-anchored lines),
grep, glob, web fetch and search (Exa, Brave or Tavily), memory, skills,
history, delegate, jobs, and any stdio MCP server you configure.

**Editors.** `construct acp` runs construct as an Agent Client Protocol
agent, so editors and other harnesses can drive it.

## Development

[AGENTS.md](AGENTS.md) is the working manual: layout, conventions, tests, and
how to run and drive the TUI against a scripted model without an API key.
Design decisions are recorded in [`docs/adr/`](docs/adr/), and benchmark
history is in [`bench/results/logs/observations.md`](bench/results/logs/observations.md).

To try a change interactively:

```sh
./scripts/try.py                            # builds, copies a sample project, opens the TUI
./scripts/try.py --project /path/to/project # use a copy of your own project
```

## License

MIT. See [LICENSE](LICENSE).
