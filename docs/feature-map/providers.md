---
id: providers
status: built
surfaces: [cli, tui]
depends: [crates/construct/src/providers.rs, crates/construct/src/endpoint.rs, crates/construct/src/cli/providers.rs]
---
# Providers

Several named endpoints, each with its own url, key and models (ADR 0027). A `[providers.<name>]` table in `config.toml` declares one; the top-level `base_url`, `api_key_secret` and `[models.*]` are the implicit provider `default`. A model is chosen as `provider/id` anywhere a model is accepted (`model =`, `--model`, `/model`, `[learn] model`, `[compaction] model`, `[agents.*] model`); a bare id goes to the one provider that lists it, else to `default`. `construct providers` lists them and `construct providers test <name>` probes one.

## Sub-features

- `providers-config` `[providers.<name>]` takes `base_url`, `api_key_secret`, `headers`, `dialect`, `cache`, `context_window`, `max_tokens` and `[providers.<name>.models."<id>"]` entries. `[providers.openrouter]` alone is enough (a built-in preset). Key: the preset's, none for a loopback url, else `<NAME>_API_KEY`; `""` is keyless. A key is sent only to the provider that declares it.
- `providers-select` `local/qwen3-8b` picks `local`; under `openrouter`, a rest with no `/` is OpenRouter's router (`openrouter/free`). An id two providers list is refused, naming both (`pick one: a/x, b/x`). The request carries the plain id.
- `providers-list` `construct providers` prints one row per provider: name, url, dialect, key (`set`, `not set` or `none`), how many models it catalogues and how old its cached list is. It sends nothing.
- `providers-test` `construct providers test <name> [--model id]` prints `models`, `reply` and `tools` lines (`ok` or `FAILED` with the reason and timing) and the window `/models` states; it exits non-zero when any check failed.
- `providers-errors` an error from a named provider starts with its name, url and key: `gpubox (http://10.0.0.5:8000/v1, key GPUBOX_API_KEY): model endpoint returned 401 …`; a missing key says `gpubox: no API key: set GPUBOX_API_KEY …`.
- `providers-status` the status line shows `provider/id`, `/status` has an `endpoint` row (`local · http://…`), and the journal's `model` rows and manifest carry `provider`.

## How to get to it (user POV)

- Add `[providers.local]` with `base_url = "http://127.0.0.1:8080/v1"` to `~/.construct/config.toml`, then `construct providers test local`, then `construct --model local/<id>` or `/model local/<id>`.

## Done when

- Each request reaches only its provider's endpoint, with only its key and the plain id; `construct providers test` names the failing step against a broken server.

## Driving it

Preconditions:

- Two scripted endpoints, as in `crates/construct-dev/tests/providers.rs`; the TUI side is the `providers-picker` recipe (see [Model picker](./model-picker.md)).

- **List and test.** Drive: `construct providers`, then `construct providers test a --model mock` against a scripted endpoint whose side reply is `ok` and whose step is a `ping` call. Expect: rows for `default`, `a` and `b`; `models  ok`, `reply   ok`, `tools   ok      called ping`, `window  131072 (stated by /models)`; the endpoint's log shows `a`'s key twice and `b` untouched.
- **Second view.** The scripted endpoints' request logs (`Env::auth_seen`, `Env::requests`): which key and which `model` each request carried.
- Contract: `crates/construct-dev/tests/providers.rs`, `crates/construct/src/providers.rs` unit tests.

## Gotchas

- `CONSTRUCT_BASE_URL` overrides only the `default` provider's url.
- OpenRouter's list fills in windows only for `default` until discovery is per provider (#129 step 4).
