---
id: providers
status: built
surfaces: [cli, tui]
depends: [crates/construct/src/providers, crates/construct/src/endpoint.rs, crates/construct/src/cli/providers.rs, crates/construct/src/llm/discover.rs, crates/construct/src/llm/overlay.rs]
---
# Providers

Several named endpoints, each with its own url, key and models (ADR 0027). A `[providers.<name>]` table in `config.toml` declares one; the top-level `base_url`, `api_key_secret` and `[models.*]` are the implicit provider `default`. A model is chosen as `provider/id` anywhere a model is accepted (`model =`, `--model`, `/model`, `[learn] model`, `[compaction] model`, `[agents.*] model`); a bare id goes to the one provider that lists it, else to `default`. `construct providers` lists them, `construct providers add` adds one and `construct providers test <name>` probes one.

## Sub-features

- `providers-config` `[providers.<name>]` takes `base_url`, `api_key_secret`, `headers`, `dialect`, `cache`, `context_window`, `max_tokens` and `[providers.<name>.models."<id>"]` entries. `[providers.openrouter]` alone is enough (a built-in preset). Key: the preset's, none for a loopback url, else `<NAME>_API_KEY`; `""` is keyless. A key is sent only to the provider that declares it.
- `providers-select` `local/qwen3-8b` picks `local`; under `openrouter`, a rest with no `/` is OpenRouter's router (`openrouter/free`). An id two providers list is refused, naming both (`pick one: a/x, b/x`). The request carries the plain id.
- `providers-list` `construct providers` prints one row per provider: name, url, dialect, key (`set`, `not set` or `none`), how many models it catalogues and how old its cached list is. It sends nothing.
- `providers-add` `construct providers add <name> --url <url> [--key-secret S] [--discover K]` probes the server, guesses `discover` (llama.cpp's `/props`, Ollama's `/api/tags`, else `/models`; a llama.cpp router with nothing loaded reads as `openai`, which its `max_model_len` serves), caches its list and appends `[providers.<name>]` to `config.toml` in place, comments kept. It refuses a taken name, a bad name, an unknown `--discover` and a server that doesn't answer (unless `--discover off`), and puts the file back if the result would not load. It never takes a key: when one is needed it prints `construct secrets set <SECRET>`.
- `providers-test` `construct providers test <name> [--model id]` prints `models`, `reply` and `tools` lines (`ok` or `FAILED` with the reason and timing) and the window `/models` states; it exits non-zero when any check failed.
- `providers-errors` an error from a named provider starts with its name, url and key: `gpubox (http://10.0.0.5:8000/v1, key GPUBOX_API_KEY): model endpoint returned 401 …`; a missing key says `gpubox: no API key: set GPUBOX_API_KEY …`.
- `providers-discover` each named provider's models and windows come from its server at startup, by `discover`: `openai` (default; `/models`, `max_model_len` before `context_length`; also `vllm`), `llama.cpp` (adds `/props`' loaded `n_ctx`), `ollama` (`/api/tags`, `/api/show` `num_ctx`) or `off`. Startup waits up to 3 s for the selected model's provider only; the list is cached per url, its models are offered as `name/id`, and it never decides where a bare id goes. A window's precedence: the user's entry, then `[providers.x] context_window`, then the list, then the top-level. `/status` says which (`· from the server's model list`).
- `providers-reach` each fetch's outcome is kept beside the cached list: `construct providers` ends a row with `· unreachable 20s ago: Connection refused (os error 111)` when the last fetch failed, and the picker marks that provider (see [Model picker](./model-picker.md)). Only startup discovery, `test` and `add` ask a server.
- `providers-shadow` a provider whose name is another list's vendor prefix (a provider `minimax` beside OpenRouter's `minimax/…`) takes those selectors; `construct run` prints `provider "minimax" takes every minimax/… selector, so N id(s) in default's models (e.g. …) are reached as default/minimax/…` on stderr, the TUI shows it as a yellow notice at startup, and `construct providers` lists it as a `warning:` line.
- `providers-status` the status line shows `provider/id`, `/status` has an `endpoint` row (`local · http://…`), and the journal's `model` rows and manifest carry `provider`; the manifest's `window_source` says which layer set the window (`catalog`, `provider`, `discovered`, `config`, `default`).

## How to get to it (user POV)

- Add `[providers.local]` with `base_url = "http://127.0.0.1:8080/v1"` to `~/.construct/config.toml`, then `construct providers test local`, then `construct --model local/<id>` or `/model local/<id>`.

## Done when

- Each request reaches only its provider's endpoint, with only its key and the plain id; `construct providers test` names the failing step against a broken server.

## Driving it

Preconditions:

- Two scripted endpoints, as in `crates/construct-dev/tests/providers.rs`; the TUI side is the `providers-picker` recipe (see [Model picker](./model-picker.md)).

- **List and test.** Drive: `construct providers`, then `construct providers test a --model mock` against a scripted endpoint whose side reply is `ok` and whose step is a `ping` call. Expect: rows for `default`, `a` and `b`; `models  ok`, `reply   ok`, `tools   ok      called ping`, `window  131072 (stated by the server)`; the endpoint's log shows `a`'s key twice and `b` untouched.
- **Add.** Drive: `construct providers add box --url http://127.0.0.1:<port>/v1/` against a scripted endpoint. Expect: `discover  openai (2 listed, windows up to 131072)`, `key       none`, and `config.toml` unchanged but for `[providers.box]` with that `base_url`; then `--model box/mock/small` runs.
- **Second view.** The scripted endpoints' request logs. Drive: `cargo test -p construct-dev --test it providers::`. Expect: every test passes; each reads `Env::auth_seen` and `Env::requests` for which key and which `model` reached which endpoint.
- Contract: `crates/construct-dev/tests/providers.rs`, `crates/construct/src/providers` unit tests.

## Gotchas

- `CONSTRUCT_BASE_URL` overrides only the `default` provider's url.
- `default` is asked for its list only while it serves the selected model, and only when it is OpenRouter (ADR 0020).
- llama.cpp behind a router: `/props` names a model file the router lists under another id, so only `max_model_len` in `/models` counts.
