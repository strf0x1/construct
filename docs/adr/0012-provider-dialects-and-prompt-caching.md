# 0012 — Provider dialects shape the body; cache breakpoints only where asked for

Status: accepted, 2026-10-22 (tier 2 under Evidence).

## Context

Issue #51: every agent step re-sends the whole conversation, so prompt caching
is the main cost lever on paid models. Providers that cache on their own
(OpenAI, DeepSeek, Grok, Moonshot, MiniMax and local llama.cpp/vLLM prefix
caches) need nothing from us. Anthropic and Gemini behind OpenRouter cache only
when the request carries `cache_control`. Construct sent none and read only
`cached_tokens`, so it could neither ask for caching nor tell whether it worked.

Construct talks to many OpenAI-compatible endpoints, each with its own quirks:
local servers that reject unknown fields, OpenRouter's extensions, and the
reasoning, token-limit and content-part differences listed in #47. We had
three options. Lift openrouter-rs's shapes into the hand-rolled client.
Depend on openrouter-rs, which only speaks OpenRouter. Or scatter
`if openrouter` branches through the client. #47 already chose to keep
`llm.rs` hand-rolled.

## Decision

- `llm/` is split by concern. `mod.rs` holds the client and the standard
  body, `stream.rs` folds the SSE stream, `usage.rs` parses accounting, and
  `dialect.rs` holds provider quirks.
- **A quirk lives only in `llm/dialect.rs`.** The client builds one standard
  body, and `Provider::shape` edits that JSON. `[provider] dialect` is
  `"auto"` (openrouter.ai means `"openrouter"`, any other host means
  `"openai"`), `"openai"` or `"openrouter"`. The `openai` dialect leaves the
  body byte-identical to before. A new quirk is a field on `Provider` and a
  few lines in `shape`, plus a dialect variant when it belongs to a new
  provider.
- **Usage parsing has no dialect.** `Usage::parse` reads every provider's
  field names: `prompt_tokens_details.cached_tokens`, DeepSeek's
  `prompt_cache_hit_tokens`, `prompt_tokens_details.cache_write_tokens` and
  `cost`. A missing field is zero (or no cost), never an error.
- **Breakpoints (OpenRouter, `[provider] cache = "auto"`):**
  - `anthropic/*` gets a top-level `cache_control: {type: ephemeral}`.
    OpenRouter then puts the breakpoint on the last cacheable block, so tool
    results get cached too. OpenRouter documents per-part breakpoints only on
    system and user messages.
  - `google/*` gets one breakpoint on the newest system or user content
    part. OpenRouter uses only the last breakpoint for Gemini.
  - Every other model gets none. `cache = "off"` turns breakpoints off.
  - No `usage: {include: true}`: OpenRouter reported `cost` and cache counts
    on main's requests without it.
- `Event::Usage` gains `cache_write` and `cost`, and `Event::TurnEnd` gains
  `cached` and `cost`. All are additive and defaulted (ADR 0008, durable
  journal). The TUI status line shows the session hit rate and $ when the
  provider reports them. The bench records `cached_tokens`,
  `cache_write_tokens`, `cache_hit_rate` and `cost_usd` per run.
  `compare.py` treats a higher hit rate as better, and treats cache writes as
  neutral because they are the price of caching and `cost_usd` already judges
  them.
- The bench reaches OpenRouter through `construct-dev serve`, which hides the
  host, so `bench.py` writes `[provider] dialect = "openrouter"` when
  `--upstream` is OpenRouter.

## Consequences

- Local and unknown endpoints see exactly the body they saw before. The
  request snapshots did not change.
- **A tool list that grows costs one full cache miss.** Providers cache tools
  first, then messages, so enabling a group changes the first block. ADR 0005
  says later turns "pay only for what grew". In fact the request right after
  an enable re-reads everything, and the cache is warm again from the next
  request. Groups that only grow (ADR 0005) still matter: a list that
  changed on every turn would miss on every turn. Compaction misses once in
  the same way.
- With `--cache` (always on in the e2e `Env`), the scripted mock acts as an
  automatic prefix cache (tools, then each message), so hit rates and prefix
  breaks show without a model (`scripts/mock.sh dir s.json --cache`). It is
  off for the bench. With it on, tier 1 `tools` wall went 73 → 111 ms,
  because the debug mock re-serializes each request and that time is billed
  to the harness. The finished-turn screen snapshot now shows `cache 47%`.
- Revisit if openai-dialect servers turn out to want breakpoints (for
  example a LiteLLM proxy in front of Anthropic). That would mean a new
  dialect, not a knob on `openai`. Also revisit if OpenRouter documents
  breakpoints on tool messages for Gemini.

## Evidence

- `crates/construct-dev/tests/e2e.rs`:
  `plain_endpoints_get_no_breakpoints_and_cache_hits_are_journaled` and
  `openrouter_anthropic_requests_carry_a_breakpoint_unless_cache_is_off`.
  The unit tests are in `llm/dialect.rs` and `llm/usage.rs`.
- Mock probe on this branch: a session with read, read, `enable_tools(web)`,
  read, read. Cached tokens per request were 0, 927/1006, 979/1059,
  **0/1232** (the request after the enable), 1204/1285 and 1257/1338.
- Shapes are from openrouter-rs (MIT, realmorrisliu/openrouter-rs:
  `CacheControl`, `PromptTokensDetails`) and OpenRouter's prompt-caching
  docs.
- Tier 2, `bench.py task --no-learn --rounds 4`, tasks stats-median,
  release-signing and edit-long-file. Main 3b16106 against this branch.
  Main's `cache_*` and `cost_usd` were filled in afterwards from its proxy
  records with the same `cache_metrics`.
  - `anthropic/claude-haiku-5.5` (`task-20261021-235623-main.json` against
    `task-20261021-235859.json`). Hit rate went from 0 to 68% median.
    stats-median 82%, release-signing 68%, edit-long-file 63%: all `better`.
    Cost per run fell on every task. stats-median $0.00454 → $0.00232 (−49%),
    release-signing $0.00206 → $0.00156 (−24%), edit-long-file
    $0.00068 → $0.00042 (−37%): all `better`. Pooled across tasks, cost is
    `same`, because the tasks' spread swamps the delta. Success (83% both),
    requests and prompt tokens are `same`.
  - `deepseek/deepseek-v4.1-flash` (`task-20261022-000230-main.json` against
    `task-20261022-000644.json`) is the control: no breakpoints are sent, and
    every metric is `same`. Hit rate is 71% on main and 77% on the branch,
    from the provider caching on its own. Success is 100% on both.
    release-signing was noisy on the branch: 2 of 4 runs took 19 and 21
    requests after the model enabled more tools.
  - The issue's ~$0.50 per run uncached was for a different, larger bench.
    Haiku 5.5 here costs $0.10/M input, $0.01/M on a cache read and
    $0.125/M on a cache write.
