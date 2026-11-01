# 0027 — Providers are named endpoints; an endpoint is resolved, never stored

Status: accepted, 2026-10-31. Lands in steps under #129: step 1 (the
`Endpoint` type, only the implicit `default` provider) in #130; step 2
(`[providers.*]`, the `openrouter` preset, `provider/model` selection, the
journal's `provider`) in #131; step 3 (`construct providers` list and
test) in #132; step 4 (discovery per provider) in #133; step 5
(`construct providers add`, the picker grouped by provider, `journal.py
errors` by provider) after it.

## Context

Issue #129 asks for several named OpenAI-compatible providers plus OpenRouter,
each with its own url, key and models, switchable from `/model`. Before it,
an endpoint was not a thing in the code. It was four top-level `Config`
fields (`base_url`, `api_key_secret`, `headers`, `[provider]`) that
`Config::select_model` overwrote from the selected model's catalog entry and
`Defaults` put back on the next switch. `AgentDef` carried a second copy
(`base_url`, `api_key_secret`). Which key applied was decided in four places:
`Config::endpoint_key`, `secrets::api_key`'s `OPENROUTER_API_KEY` fallback,
`Agent::build` and `AgentDef::inherits_key`. Whether a switch needed a new
client was a comparison of a four-field tuple in `Agent::set_model`. Every
endpoint feature #129 adds (a name, discovery, a status line) would have been
threaded through all of that.

OpenAI and OpenRouter already shared everything on the wire: one `Client`,
one SSE parser, one `Usage::parse` (ADR 0012). What differs between them is
data: a host, a default key name, cache breakpoints, and whether `/models`
states windows (ADR 0020). oh-my-pi models providers as `providers.<id>`
with a typed `discovery`, and 13 wire APIs; #129 lists what to take and what
not to.

## Decision

1. **An `Endpoint` is computed, never stored.** `crate::endpoint::Endpoint`
   is the url, key secret, headers and dialect one request goes to.
   `Config::endpoint()` resolves it from what the user declared and the
   selected model's catalog entry on each call. The declared fields are
   never overwritten by a model switch, so nothing goes stale and there is
   nothing to restore. A delegated child is handed its endpoint as its own
   declaration (`Config::pin_endpoint`).
2. **A key is chosen in one place: `Endpoint`.** `key()` for a session,
   `env_key()` for a child, `require()` to fail before sending when a
   declared secret resolves to nothing. A key is sent only to the endpoint
   that declares it (#65). The `OPENROUTER_API_KEY` fallback belongs to the
   `default` endpoint alone, and never to one whose `api_key_secret` is
   empty: empty means keyless.
3. **Providers are `[providers.<name>]` tables** (step 2). The top-level
   fields and `[models.*]` / `models.toml` are the implicit provider
   `default`, so existing configs and the golden request snapshots do not
   change. A provider's own model entries nest under it
   (`[providers.x.models."id"]`) and need no `base_url`.
4. **A built-in provider is a row of data, not a code path.** A preset
   (name, url, default secret, hosts, dialect, discovery) is one entry in a
   table. `openrouter` is the first. A preset is present only when declared
   (`[providers.openrouter]` alone is enough): a key that happens to
   resolve does not move where an existing config's requests go.
5. **Selection is `provider/model`, decided without discovery.** A
   selector whose first segment names a configured provider picks it, and
   the rest is the id. Otherwise the whole selector is a bare id: unique
   across providers resolves, ambiguous is an error listing the candidates.
   The answer never depends on whether a model list is cached. OpenRouter
   ids are always `vendor/model`, so under the `openrouter` provider a rest
   with no `/` is OpenRouter's own router: `openrouter/free` is the id
   `openrouter/free`, and `openrouter/anthropic/x` is `anthropic/x`.
6. **Discovery is a typed enum, one parser per server shape**
   (`openai`, `llama.cpp`, `vllm`, `ollama`, `off`), each pinned by a
   recorded fixture. Local servers are `discover` values only, not presets
   with default ports: a guessed port reaches the wrong server silently. A
   window's precedence is the user's model entry, then the provider's
   `context_window`, then the discovered value, then the top-level
   `context_window`; a window stated in an overflow error still wins for
   the session (ADR 0013). Discovery reads the cache on the UI path and
   refreshes in the background (ADR 0020). A named provider discovers with
   `openai` unless it says otherwise; `default` keeps ADR 0020's rule (only
   when it is OpenRouter) and is asked only while it serves the selected
   model, so moving to a local provider stops requests to openrouter.ai.
   llama.cpp's `/props` describes only the loaded model: its `n_ctx` caps a
   listed model only when `/models` lists one, or one whose id is the
   loaded alias. Each fetch's outcome is written beside the cached list
   (when it worked, or when and why it failed), and that record is what
   the picker and `construct providers` show as reachability: nothing on
   the UI path asks a server. A list that lands after the session started
   is laid in without re-selecting the running model, so the UI's window
   never moves away from the agent's.
7. **No `Provider` trait.** All variation between OpenAI-compatible
   providers is data (presets), body edits (`llm::dialect`, ADR 0012) or a
   discovery parser. A trait arrives with the second wire protocol
   (Anthropic Messages, OpenAI Responses), at the level of the wire: build
   the body, fold the stream.
8. **The journal records the provider.** `Event::Model` and the manifest
   (in the `head` row and each `manifest` event) gain a defaulted
   `provider`, absent for `default` (additive, ADR 0008). Usage rows belong
   to the model row before them, so they carry none.

## Consequences

- Adding a provider: a user writes a `[providers.x]` table, no code. A
  built-in one is a preset row and a fixture test; a quirk is a dialect
  field (ADR 0012); a new `/models` shape is a `discover` variant and its
  parser. Nothing outside `llm/` and `endpoint.rs` names a provider.
- `construct config` prints the declared top-level fields, not the selected
  model's endpoint, which it used to mix in. The resolved endpoint is what
  the manifest, `/status` and the picker show. The manifest's
  `config_sha256` changes for a config whose selected model names its own
  endpoint.
- A config with `api_key_secret = ""` no longer sends `OPENROUTER_API_KEY`
  to its endpoint. That was a leak of the kind #65 closed for children.
- Revisit 5 if users mostly type OpenRouter ids under a named `openrouter`
  provider and the router rule surprises them; revisit 7 when a second wire
  protocol is in scope.

## Evidence

- Issue #129 (proposal, open questions 1–4 answered by decisions 5, 3, 6
  and 6); #65 (a key sent where it was not declared); ADR 0012, 0019, 0020.
- Step 1: `crates/construct/src/endpoint.rs` unit tests, the `models.rs` and
  `agents.rs` tests rewritten against `Config::endpoint()`, and the golden
  request snapshots unchanged (`cargo xtask check`).
- Step 2: `crates/construct/src/providers.rs` unit tests (resolution, the
  router rule, key defaults, mistakes); `crates/construct-dev/tests/providers.rs`
  (two scripted endpoints switched with `/model`, each request carrying only
  its own key and the plain id; an ambiguous bare id refused before any
  request; a loopback provider sent no key; a child on another provider);
  the `providers-picker` verify recipe; golden snapshots unchanged.
- Step 4: `llm::discover` parsers pinned to `crates/construct/tests/fixtures/discover/`
  (llama.cpp's `/v1/models` and `/props` recorded from a llama.cpp server
  behind a router; vLLM and Ollama written from their documented shapes);
  the layering test in `llm/overlay.rs`; e2e
  `a_named_providers_model_list_sets_the_window_at_startup_and_is_cached`.
  Against that llama.cpp server, `model = "local/strata-flash-next"` with
  no window configured planned against 262144, the window its `/models`
  states as `max_model_len`.
