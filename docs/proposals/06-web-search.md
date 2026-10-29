# Proposal 6 — Web search that says what went wrong and does more per call

Status: step 1 in progress (this branch); steps 2–5 not started. Written
2026-10-28 against `aa49457`. Evidence and sources:
[`docs/research/web-search-tooling.md`](../research/web-search-tooling.md).

`web_search` works on a fresh install (DuckDuckGo scrape) and upgrades to Exa
or Brave when a key resolves. Three things hold it back: failures read as
"no results", the model gets a 400-character snippet and no dates, and the
only knobs are `query` and `limit`. The research note surveys the market;
its finding that matters here is that quality at the top tier is a tie
(Brave, Exa, Parallel, Firecrawl, Tavily are within noise in the independent
measurements), so we choose backends on price, API shape and privacy, not on
a leaderboard. Google's and Bing's search APIs are closed to new customers,
so "Google for free" is not on the table.

## Steps

1. **Honest errors, a current Exa call, Tavily.** Every backend goes through
   one helper that checks the HTTP status: a rejected key names the secret to
   fix, a spent quota says so, a short 429 `Retry-After` is retried once, a
   long one is reported with the wait. Exa's deprecated `numSentences` becomes
   `maxCharacters`. Tavily (`TAVILY_API_KEY`) is the third backend in the
   first-key-wins order Exa, Brave, Tavily. The search code moves out of
   `tools/web.rs` into `tools/web/{search,http,ddg}.rs`. Done when: unit tests
   cover status mapping, the 429 retry and each vendor's response mapping, and
   the golden request snapshots are unchanged (no schema change).
2. **More per result.** Show a date when the backend gives one, raise the
   snippet to about 1,200 characters for the first five results and 300 for
   the rest, name the backend that answered in the output, and add two
   optional arguments: `recency` (`day|week|month|year`) and `domains` (host
   list), each mapped to the backend's native filter and ignored with a note
   where it cannot be. The routed-web snapshot changes by design (about
   150–250 bytes of schema); accept it with `cargo xtask snapshots`.
3. **Explicit selection.** `[web] search = "auto" | "exa" | "brave" |
   "tavily" | "ddg"` and `CONSTRUCT_WEB_SEARCH` for one run. `auto` keeps the
   step 1 order. Fall through to the next backend only on errors that say
   nothing about the query (network, 5xx, a 429 that outlasted the retry),
   never on 401/402/403/432/433, so a bad key or spent quota is shown instead
   of silently sending the query to a vendor with a different retention policy.
4. **A better `web_fetch`.** Today it is a plain GET with no JS rendering,
   bot handling or PDF support. Try, in order of cost: content negotiation
   (already sends `Accept: text/markdown`), a keyed provider's extract endpoint
   (Tavily `/extract`, Exa contents) when the plain fetch fails or comes back
   thin. No readability crate until a bench task shows the need.
5. **Pick defaults with a real model.** `bench.py task --arm` over search
   backends and snippet sizes on tasks that need a current fact, scored by
   `check.sh` on the work dir. Only this step may reorder `auto`.

## Not doing

- A `provider` argument on the tool: it costs first-request bytes and lets the
  model choose where code-adjacent queries go.
- Server-side search (OpenAI, Anthropic, Gemini grounding, OpenRouter's web
  plugin): it runs outside the tool loop, so the journal, the permission
  model and the router never see it.
- Hosted MCP search endpoints: the MCP client is stdio-only (ADR 0011), and a
  direct HTTP call is smaller than the transport it would need.
- Making the anonymous vendor endpoints (Exa, Parallel MCP) the keyless
  default. DuckDuckGo stays, and the trade is documented; an opt-in is a
  possible later step.

## Privacy

Every query goes to the vendor that answers, and models write queries from
repository context. Retention is mostly unpublished; Tavily's policy says
queries may be used to improve its responses and shared with third-party index
providers. Step 2 puts the backend's name in the output so the user can see
where a query went, and the key descriptions in `secrets.rs` stay neutral
until a vendor's terms are verified.
