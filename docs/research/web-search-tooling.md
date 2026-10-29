# Web search and fetch backends for construct

Research note, accessed **2026-10-28**. Not a proposal and not an ADR: it is
evidence for one. `docs/research/` is new; it is the home for survey-style
notes that inform a proposal (`docs/proposals/`) or decision (`docs/adr/`)
but commit to neither. No code was changed.

Legend on every claim: **[V]** verified by fetching the primary source (or a
live probe) on the date above; **[VC]** vendor-claimed (the vendor's own
number, not independently measured); **[S]** secondary source (blog, third-party
site), never relied on for facts the vendor owns; **[U]** unverified.
No API keys were present in the environment (checked variable names only), so
**no authenticated API call was made**; every response shape below is from
documentation, not from a live response. Keyless probes were made and are
listed in section 7.

---

## 1. Summary and recommendation

**The state of the art in one paragraph.** The market has converged on "search
that returns LLM-ready page excerpts in the same call" (Exa highlights,
Parallel excerpts, Perplexity `snippet`, Brave LLM Context, Tavily
`raw_content`). Prices cluster at **$4-8 per 1k queries** for the established
players and **$1 per 1k** for Parallel `turbo`/`fast` and Perplexity `fast`.
Google's Custom Search JSON API is **closed to new customers and sunsets
2027-01-01** [V], and the Bing Search API was **retired 2025-08-11** [V]; there
is no first-party, self-serve, raw Google or Bing web-search API left. "Google
for free" is not available except as model-bound grounding (Gemini: 5,000
free grounded requests a month on Gemini 3+, then $14/1k [V]).

**Quality.** Independent evidence is thin and says the top tier is a tie.
The best independent measurement I found (AIMultiple, 100 queries, 8 APIs, LLM
judge, Dec 2025 snapshot) has Brave, Firecrawl, Exa and Parallel Pro
statistically indistinguishable, Tavily fifth within noise, Perplexity and
SerpApi last [S]. An arXiv preprint (2607.10198, Brave vs
Tavily vs Firecrawl, 100 SealQA-Hard questions) found 25/25/26 correct:
"similar accuracy" [V]. Every vendor-published "we win" chart is marketing and
mostly disagrees with the others. Choose on price, API shape, privacy and
ergonomics, not on a leaderboard.

**Recommendation (ranked).**
1. **Exa: keep as the default paid backend, fix it.** The user likes it,
   $10 of free credit a month (2,500 `lite` searches) needs no card [V], and
   one call returns highlights. The current call sends `numSentences`, which
   Exa now lists as **deprecated** [V]; and neither `exa()` nor `brave()`
   checks the HTTP status, so a bad key or a 429 surfaces as "no results".
2. **Brave: keep as the second backend.** $5/1k, $5 free credits a month
   (the page also says "up to $20" and does not reconcile the two) [V], 50
   QPS, own index, tied for first in the one independent leaderboard [S].
   Its new LLM Context endpoint returns extracted chunks per URL (price
   page does not list it separately: [U]).
3. **Tavily: add as the third backend.** The user finds it cheap and good.
   1,000 free credits a month, $0.008/credit pay-as-you-go ($8/1k basic,
   down to $5/1k on the $500 plan) [V]; a clean bearer-auth JSON API with
   `time_range`, domain lists, `topic=news`, `include_raw_content` and a
   separate `/extract`. Caveats: Nebius acquired Tavily (Feb 2026) [V]; its
   privacy policy lets it use query data to improve responses and share
   queries with third-party index providers such as Google [V].
4. **Parallel (and Perplexity Search) as cheap-and-compressed options, only
   after a bench run.** Parallel `fast` is $1/1k and `/extract` is $1/1k [V];
   Perplexity Search is $5/1k, $1 `fast`, 50 QPS flat [V]. Their independent
   numbers are mixed (section 4). Not worth a code path before measurement.
5. **Not recommended as primary:** SERP scrapers (Serper, SerpApi,
   SearchApi: they scrape Google; Google sued SerpApi in Dec 2025 [S]),
   Kagi ($12/1k [V], strong product, 3x the price), Google CSE, Bing,
   server-side/native search (OpenAI/Anthropic $10/1k plus tokens [V]),
   OpenRouter's `:online` (deprecated plugin; see 6.6).

**Keyless default.** Nothing keyless is verifiably reliable or documented as
ToS-safe. DuckDuckGo's HTML endpoint returned 10 results today [V probe] but
is an undocumented UI endpoint; `duckduckgo.com/robots.txt` disallows `/html`
while `html.duckduckgo.com/robots.txt` allows all [V]. Public SearXNG
instances were useless: 0 of 6 returned JSON (429 or HTML) [V probe]. The
better keyless candidates are the **vendors' own free anonymous MCP
endpoints**, which both worked today with plain JSON-RPC POSTs: Exa
`https://mcp.exa.ai/mcp` (0.97 s) and Parallel `https://search.parallel.ai/mcp`
(0.60 s, forced to `fast` mode when anonymous) [V probe]. Vendor docs offer
them explicitly as free and rate-limited [V]. They are not an API contract (no
SLA, limits unpublished), so keep DDG as the last resort rather than replace
it. construct's MCP client is stdio-only (ADR 0011) so these need a ~40-line
direct JSON-RPC call, not `[mcp.*]` config.

**Minimal first step (one PR, no new dependencies, about 150 lines in
`tools/web.rs` plus one line in `secrets.rs`):**
(a) check HTTP status in every backend and map 401/402/403/429/432/433 to a
prose error naming the key to fix, with one `Retry-After`-aware retry on 429;
(b) replace Exa `numSentences` with `highlights.maxCharacters` and lift the
400-char snippet cap to ~1,200 chars for the top 5 results, printing the
published date when present; (c) add Tavily (`TAVILY_API_KEY`) behind a small
`[web] search = "auto" | "exa" | "brave" | "tavily"` setting, `auto` keeping
today's first-key-wins order; (d) add two optional tool args, `recency`
(`day|week|month|year`) and `domains` (include list), mapped per backend;
(e) a `web_fetch` fallback for the failures the in-process GET cannot fix
(403/429/503, JS shell, PDF) through whichever of Exa `/contents`, Tavily
`/extract` or Parallel `/extract` has a key. Then settle provider order with a
tier-2 `bench/tasks/` web-research task using arms, per AGENTS.md. Do not add
a readability crate yet (see 5.3).

**What I could not verify:** any authenticated response shape or latency;
Exa/Brave/Tavily/Parallel behaviour under rate limiting; Brave LLM Context
price; Serper paid price (pricing page 404); Parallel and Perplexity free
tiers; whether OpenRouter `openrouter:web_search` results are visible to
construct's own tool loop; ToS wording for anonymous MCP use and for DDG's
Acceptable Use Policy (not read).

---

## 2. Comparison table

Price is per 1,000 search requests, entry/base tier, USD unless noted. "Content"
= returns page text or excerpts in the search call. Quality column says what
evidence exists and its provenance; see section 4 for the numbers.

| Provider | Quality evidence | Content in search? | $/1k | Free tier | Key/ToS notes | Effort in web.rs |
|---|---|---|---|---|---|---|
| **Exa** | AIMultiple 3rd of 8, tie-top-4 [S]; openbenchmarks leads factual and hard retrieval [S, undisclosed funding]; vendor evals [VC] | Yes: `contents.highlights/text` included in base price (up to 10 results) [V] | `lite`/`instant` 4; `auto`/`fast` 7; deep 12-15 | $10/mo credit, resets monthly, no card [V] | `x-api-key`; ZDR Enterprise-only [V]; free anonymous MCP [V] | Exists. Fix params |
| **Tavily** | GAIA 64% vs Exa 58% vs Brave 57% (2025, single run) [V paper]; AIMultiple 5th [S]; arXiv 2607.10198 tie [V] | Optional `include_raw_content` (markdown/text); `content` chunks always | 8 basic (PAYG), 5 on Growth; advanced 2x | 1,000 credits/mo, no card [V] | Bearer; owned by Nebius [V]; no ZDR listed; may share queries with index providers [V] | ~35 lines |
| **Brave** | AIMultiple 1st (tie) [S]; Brave's own SimpleQA claim [VC] | `extra_snippets`; separate LLM Context endpoint with chunks | 5 (Search); Answers 4 + tokens | $5 credit/mo (page also says "up to $20") [V]; card required | `X-Subscription-Token`; own index; storage rights depend on plan [U which]; ZDR Enterprise [V] | Exists; +LLM Context ~30 lines |
| **Parallel** | openbenchmarks mid-high [S]; own WISER evals [VC] | Yes: `excerpts[]` markdown, `max_chars_per_result` | 1 (turbo/fast); 5 (basic/advanced) | [U]; keyless MCP [V] | `x-api-key`; separate Extract $1/1k [V] | ~30 lines |
| **Perplexity Search** | openbenchmarks top on search-only hard retrieval [S]; AIMultiple 7th, 11s+ latency [S]; own search_evals [VC] | Yes: `snippet` extracted text, `max_tokens_per_page` | 5; `fast` 1 | none stated [U] | Bearer; 50 QPS, not tiered [V] | ~30 lines |
| **Firecrawl** | AIMultiple 2nd (tie) [S]; arXiv 2607.10198 [V] | Optional `scrapeOptions` (markdown) per result | 2 credits/10 results: ~7.6 (Hobby) to ~2 (Standard) | 1,000 credits/mo; 10 req/min [V] | ZDR via `enterprise:["zdr"]` at 5x cost [V]; also scrape/crawl/PDF | ~40 lines |
| **Linkup** | openbenchmarks lower on hard retrieval [S]; own F-score claim [VC] | `searchResults` content; `sourcedAnswer` | 5 standard; 50 deep | $20/mo credit with professional email [V] | Bearer; fetch endpoint with `renderJs` [V] | ~30 lines |
| **You.com** | openbenchmarks lower-mid [S] | Yes (snippets; Contents API separate) | 5; contents 1/1k pages | $10 credit; keyless MCP 100/day [V] | | ~30 lines |
| **Kagi** | none found [U] | `/search` can return full markdown of top results [V] | 12; Extract 4 | none stated [U] | `Authorization: Bot` / Bearer, `/api/v1`; per-account settings | ~30 lines |
| **Mojeek** | none found [U] | snippets only | £2 (Startup) / £3 | trial credit; card needed [V] | own crawler, AI use allowed; 90-day retention, ZDR on request [V] | ~25 lines |
| **Serper** | Google results [V]; no head-to-head found | snippets | [U] (pricing 404) | 2,500 free queries [V] | scrapes Google: ToS/legal exposure [S] | ~25 lines |
| **SerpApi** | AIMultiple last [S] (SerpApi is a disclosed subscriber of that firm) | snippets | 10-25 by plan | 250/mo; 50/hr [V] | Google v. SerpApi, DMCA 1201, filed 2025-12-19 [S] | ~25 lines |
| **SearchApi** | none | snippets | 4 at 10k plan | 100 on sign-up [V] | scrapes Google | ~25 lines |
| **Jina** `s.`/`r.` | none independent; `r.jina.ai` used as fetch backend in arXiv 2607.10198 [V] | `s.jina.ai`: top 5 with content [VC] | search >= 10k tokens/request [V] | 10M tokens per new key; `s.` rejects keyless (401) [V probe] | Elastic-owned; ReaderLM-v2 is CC-BY-NC [V] | Reader: ~10 lines |
| **SearXNG** (self-host) | aggregates Google/Bing/etc. | snippets (`content`) | free | free | JSON must be enabled in `settings.yml` [V]; public instances unusable [V probe] | ~25 lines |
| **DuckDuckGo HTML** | none | snippet | free | free | no API; robots split [V]; AUP not read [U] | Exists |
| **Marginalia** | niche indie index | snippet | free non-commercial key by email; commercial metered | `public` key often rate-limited [V] | CC-BY-NC-SA for free keys [V] | ~20 lines |
| **Google CSE JSON** | n/a | snippets | 5 | 100/day | **Closed to new customers; ends 2027-01-01** [V] | do not build |
| **Gemini grounding** | n/a | model-bound | 14 (Gemini 3+), 35 (2.5) | 5,000/mo (Gemini 3+) [V] | not a search API; Search Suggestions display terms [U] | n/a |
| **Bing** | n/a | n/a | n/a | n/a | **Retired 2025-08-11**; replacement is Azure AI Agents "Grounding with Bing" (model-bound) [V] | n/a |
| **OpenAI / Anthropic native** | n/a | model-bound, encrypted/cited | 10 + tokens | none | Anthropic ZDR-eligible, dynamic filtering not [V] | n/a (server-side) |
| **OpenRouter `:online`** | n/a | via Exa/Parallel/Perplexity/Firecrawl/native | engine price + tokens | none | plugin deprecated, server tool beta [V] | dialect field, not a tool |

---

## 3. Per-provider notes

### 3.1 Exa
- Pricing [V, https://exa.ai/pricing, md form]: `/search` `lite` and `instant` $4/1k, `auto`/`fast` $7/1k, deep $12-15/1k; base price "includes up to 10 results", each extra $1/1k; AI page summaries +$1/1k pages; `/contents` $1/1k pages **per content type** (text + highlights on one page counts twice). So search-with-highlights needs no separate contents bill; a standalone `/contents` fetch of a known URL does. New accounts get $10 credit, reset to $10 monthly (2,500 Lite searches), no card. Rate limits not published; Enterprise has custom QPS and ZDR.
- API [V, https://exa.ai/docs/reference/search]: `POST /search`, header `x-api-key`. `type`: `auto` (default), `lite`, `deep-lite`, `deep`; `instant`/`fast`/`deep-reasoning` are legacy. `numResults` 1-100. `category`: company, publication, news, personal site, financial report, people. `includeDomains`/`excludeDomains` (up to 1,200, hostname, path prefix or wildcard). `startPublishedDate`/`endPublishedDate` ISO 8601. `contents`: `text` (`maxCharacters`, `verbosity`), `highlights` (`maxCharacters`; `numSentences` and `highlightsPerUrl` **deprecated**), `summary`, `maxAgeHours` (0 = always live crawl, -1 = cache only). `lite` accepts only a limited set that still includes `category`, domains, dates, `highlights`, `text`. Response: `results[].{title,url,id,publishedDate,author,highlights,text,summary}`, plus `costDollars` and `searchTime`.
- Latency (vendor/reseller-claimed, https://openrouter.ai/docs/guides/features/server-tools/web-search): `instant` ~250 ms, `fast` ~450 ms, `auto` ~1 s, `deep-lite` ~4 s, `deep` 4-15 s [VC]. Exa docs themselves give only "consistent 4-second latency" for `deep-lite` [V]. My single anonymous MCP call: 0.97 s [V probe].
- Hosted MCP: `https://mcp.exa.ai/mcp`, keyless free rate-limited, OAuth or `x-api-key` for your quota; tools `web_search_exa`, `web_fetch_exa`, opt-in `web_search_advanced_exa` [V, https://exa.ai/docs/reference/exa-mcp].
- ZDR: Enterprise plans, enabled per team, covers Search/Contents/Answer/Agent [V, https://exa.ai/docs/reference/zero-data-retention]. Default query retention and training use: not stated there [U].

### 3.2 Tavily
- Credits [V, https://docs.tavily.com/documentation/api-credits]: search basic 1, advanced 2; extract 1 credit per 5 successful URLs (advanced 2); map 1 per 10 pages; crawl = map + extract; research 4-250. Free "Researcher": 1,000 credits/mo, no card. PAYG $0.008/credit. Plans: Project $30/4,000 ($0.0075), Bootstrap $100/15,000, Startup $220/38,000, Growth $500/100,000 ($0.005). Failed extractions are not charged.
- Rate limits [V, https://docs.tavily.com/documentation/rate-limits]: default 100 RPM dev, 1,000 RPM production (production needs a paid plan or PAYG); crawl 100; research 20.
- API [V, https://docs.tavily.com/documentation/api-reference/endpoint/search]: `POST https://api.tavily.com/search`, `Authorization: Bearer tvly-...`. `query`, `search_depth` (`basic` default, `advanced`, `fast`, `ultra-fast`), `max_results` 0-20, `topic` (general/news/finance), `time_range` (day/week/month/year), `start_date`/`end_date`, `include_domains` (300) / `exclude_domains` (150), `include_raw_content` (`true|markdown|text`), `include_answer`, `country`, `max_age_hours`, `auto_parameters`. Response `results[].{title,url,content,score,raw_content,published_date}`, `answer`, `usage.credits`, `response_time`. Errors: 401, 422, 429 (+`Retry-After`), **432** key/plan limit, **433** PAYG limit, 434 flex capacity. No documented credit surcharge for `include_raw_content` on the credits page (absence verified, not a guarantee).
- Extract [V, .../endpoint/extract]: `POST /extract`, up to 20 URLs, `extract_depth` basic/advanced, `format` markdown/text, optional `query` reranks chunks, `failed_results[]` per URL. Whether it renders JS or parses PDFs is **not stated** on that page [U].
- Corporate/privacy: Nebius announced (2026-02-10) and closed (2026-02-19) the acquisition [V, https://nebius.com/newsroom/nebius-announces-agreement-to-acquire-tavily-to-add-agentic-search-to-its-ai-cloud-platform and Nebius 6-K on sec.gov]; price figures in press are [U]. Privacy policy (updated 2025-11-24) [V, https://www.tavily.com/privacy]: query data kept "for the period necessary to provide and improve the Services", portions may be used "to improve our responses to future queries", may be shared with third-party index providers (e.g. Google) if Tavily's own index cannot answer; no ZDR option mentioned. Check this before sending code-adjacent queries.

### 3.3 Brave Search API
- Plans [V, https://brave.com/search/api/]: Search $5/1k, 50 QPS, Goggles and extra snippets; Answers $4/1k + $5/M tokens, 2 QPS; Enterprise with "full-funnel Zero Data Retention". Prepaid credits, card required. Free: "$5 in free credits every month" on plan cards, "up to $20" in the hero [V, unreconciled]. Storage "with a plan that includes storage rights" - which plan is not stated [U].
- Web Search [V, https://api-dashboard.search.brave.com/app/documentation/web-search/get-started]: `GET https://api.search.brave.com/res/v1/web/search`, header `X-Subscription-Token`. `q` (operators like `site:` inside), `count` (max 20, default 20), `offset` (max 9), `freshness` (`pd|pw|pm|py` or `YYYY-MM-DDtoYYYY-MM-DD`), `country`, `search_lang`, `safesearch`, `extra_snippets=true` (up to 5 more excerpts), `goggles`. Response `web.results[].{title,url,description,extra_snippets}`; descriptions contain `<strong>` tags (the current code strips them).
- LLM Context [V, https://api-dashboard.search.brave.com/documentation/services/llm-context]: `GET|POST https://api.search.brave.com/res/v1/llm/context`, same auth; `q`, `count`, `freshness`, `maximum_number_of_tokens` (default 8192, 1,024-32,768), `maximum_number_of_urls`, `maximum_number_of_tokens_per_url` (default 4,096), `context_threshold_mode`, `goggles`. Response `grounding.generic[].{url,title,snippets[]}` plus `sources` metadata. Snippets may be plain text or JSON-serialized tables/code. This is a search+content call. Price and plan are not on the pages I fetched [U]; the plans page says Search includes "additional LLM context" [V], which suggests the $5/1k plan.
- Docs say respect a 1-second sliding-window rate limit [V].

### 3.4 Google
- Custom Search JSON API [V, https://developers.google.com/custom-search/v1/overview]: 100 free queries/day, $5/1k beyond, 10,000/day cap; **"closed to new customers"**; existing customers must move by **2027-01-01**; no whole-web search (Vertex AI Search can search up to 50 domains; whole-web needs a separate arrangement). Do not build on it.
- Gemini "Grounding with Google Search" [V, https://ai.google.dev/gemini-api/docs/pricing]: Gemini 3+ 5,000 free requests/month shared across models, then $14/1k search queries; Gemini 2.5 1,500/day free then $35/1k grounded prompts. It is a model tool (`google_search` in the request), not a result list; display requirements ("search_suggestions" HTML) live in the ToS, which I did not read [U]. URL context is free apart from input tokens [V]. Via OpenRouter, Google models use it natively (6.6).
- "Google if it were free": the only free Google-quality path is Gemini grounding's free quota, usable only when the chat model is Gemini. Serper/SerpApi/SearchApi resell scraped Google; Google sued SerpApi under DMCA 1201 on 2025-12-19 [S: ipwatchdog.com, searchengineland.com], no ruling found.

### 3.5 Bing
Retired 2025-08-11, no new sign-ups; replacement is "Grounding with Bing Search" inside Azure AI Agents [V, https://learn.microsoft.com/en-us/lifecycle/announcements/bing-search-api-retirement]. Model-bound, not a raw API; price unverified.

### 3.6 SERP scrapers
- Serper [V, https://serper.dev]: Google results, 1-2 s claimed, 2,500 free queries, no card. Paid price: pricing page returned 404 [U].
- SerpApi [V, https://serpapi.com/pricing]: free 250/mo (50/hr); Starter $25 for 1,000; Developer $75/5,000; Production $150/15,000; $2,750/500k.
- SearchApi [V, https://www.searchapi.io/pricing]: 100 free; Developer $40/10,000; 20% of plan credits per hour.
- All three are Google-scraping intermediaries: legal exposure is theirs, but continuity risk is yours. No head-to-head quality evidence beyond AIMultiple's SerpApi (last place) [S].

### 3.7 Perplexity
- Search API [V, https://docs.perplexity.ai/docs/search/quickstart and .../getting-started/pricing]: `POST https://api.perplexity.ai/search`, Bearer. `query` (string or up to 5), `max_results` 1-20, `max_tokens`, `max_tokens_per_page`, `country`, `search_domain_filter` (allow, or deny with `-`, max 20, no mixing), `search_language_filter`, `search_type: "fast"`. Response `results[].{title,url,snippet,date,last_updated}`; no generated answer. $5/1k, `fast` $1/1k; multi-query counts as one billing unit but N rate-limit units. Rate limit **50 query units/s, independent of tier** [V, .../rate-limits-usage-tiers]. A recency parameter was not visible in the part I read [U].
- Agent API `web_search` tool $2.50/1k, `fetch_url` $0.0005/call [V]. Sonar model token prices: not fetched [U]; Sonar is an answer-generating model, a poor fit for a tool that returns results.

### 3.8 Parallel
- Pricing [V, https://docs.parallel.ai/getting-started/pricing]: Search $1/1k (`turbo`/`fast`) or $5/1k (`basic`/`advanced`), 10 results included, +$1/1k beyond; **Extract $1/1k URLs**; Task $5-$2,400/1k. Free tier and rate limits not on that page [U].
- API [V, https://docs.parallel.ai/api-reference/search-api/search]: `POST https://api.parallel.ai/v1/search`, `x-api-key`. Required `search_queries[]` (2-3 queries of 3-6 words recommended); optional `objective`, `mode` (`turbo|fast|basic|advanced`, default `advanced`), `max_chars_total`, `advanced_settings.{source_policy.{include_domains,exclude_domains,after_date}, fetch_policy.{max_age_seconds>=600,timeout_seconds}, excerpt_settings.max_chars_per_result, location, max_results}`. Response `results[].{url,title,publish_date,excerpts[]}`. The query shape (objective plus keyword list) fits a model that writes both, but costs a larger tool schema.
- Latency (reseller-claimed [VC]): turbo ~200 ms, fast ~550 ms, basic ~1 s, advanced ~3 s. Docs: Fast "within a 1-second latency budget" [VC]. My anonymous MCP call: 0.60 s [V probe].
- Anonymous MCP `https://search.parallel.ai/mcp` works without an account at lower limits (numbers unpublished), forced `fast` mode, excerpts capped ~25k chars per call [V, https://docs.parallel.ai/integrations/mcp/search-mcp].

### 3.9 Firecrawl
- [V, https://docs.firecrawl.dev/billing, https://www.firecrawl.dev/pricing]: scrape 1 credit/page, search 2 credits per 10 results (+ scrape cost if scraping results), JSON mode +4, PDF parsing 1 credit per PDF page, ZDR search `["zdr"]` 10 credits/10 results. Plans: Free 1,000 credits/mo (10 req/min search); Hobby $19 (5,000); Standard $99 (100,000; 500/min); Growth $399 (500,000). Derived search cost: ~$7.6/1k searches on Hobby, ~$2/1k on Standard.
- Search API [V, https://docs.firecrawl.dev/features/search]: `query`, `limit`, `sources` (web/news/images), `categories` (research/pdf/developer/gov), `tbs` (e.g. `qdr:w`), `location`, `includeDomains`/`excludeDomains`, `scrapeOptions` (e.g. `formats:["markdown"]`). Note: `research` category changes on 2026-11-16.
- Strength is as a fetch backend (JS rendering, anti-bot, PDF); search is comparable. Through OpenRouter it is $0.005/search + $0.0125 per result [V], far costlier than direct.

### 3.10 Jina
- Reader `r.jina.ai/<url>`: **works keyless**, 200 with markdown today [V probe]; documented 20 RPM keyless, 500 RPM with a free key, 5,000 premium; headless-browser engine by default, `X-Engine: direct` for plain fetch, native PDF, selectors, cookie forwarding [VC, https://jina.ai/reader/]. Responses carry `x-usage-tokens`. Search `s.jina.ai`: keyless request returned **401** [V probe]; docs say blocked without a key, 100 RPM with one, search costs at least 10,000 tokens per request, 10M free tokens per new key [VC]. The site is now branded Elastic; ReaderLM-v2 is CC-BY-NC [VC]. Privacy: the URL (and whatever the page contains) goes to Jina.

### 3.11 You.com, Linkup, Kagi, Mojeek
- You.com [V, https://you.com/pricing]: Search $5/1k, Contents $1/1k pages, Research API $12-1,200/1k; $10 free credit; 100 queries/day keyless via MCP.
- Linkup [V, https://docs.linkup.so/pages/documentation/development/pricing]: search $0.005 (standard/fast/flash) or $0.05 (deep); fetch $0.001 (standard, no JS), $0.005 (`renderJs`), $0.005/$0.01 (pro); $20 monthly top-up for professional emails; 429 on exhausted balance. API [V]: `POST /v1/search`, `q`, `depth`, `outputType` (`searchResults|sourcedAnswer|structured`), `includeDomains`, `excludeDomains`, `fromDate`, `toDate`, `maxResults`.
- Kagi [V, https://help.kagi.com/kagi/api/search.html, https://kagi.com/api/pricing]: `POST https://kagi.com/api/v1/search` and `/extract` (10 URLs); Search $12/1k, Extract $4/1k; billing set up at kagi.com/api; whether a Kagi subscription is required: [U]. Honours account Lenses and blocklists.
- Mojeek [V, https://www.mojeek.com/services/search/web-search-api/]: own crawler; Startup £2/1k (5 QPS, 100k/day, cache 1 hour), Business £3/1k, AI usage allowed on all plans, standard 90-day retention with ZDR on request; card needed for the trial. The search-result snippet mentioned a cheaper "Personal" tier that the fetched page did not list [U].

### 3.12 SearXNG, DuckDuckGo, Marginalia
- SearXNG [V, https://docs.searxng.org/dev/search_api.html]: `GET /search?q=...&format=json` works only if `json` is in `search.formats` in `settings.yml`, else **403**; params `categories`, `language`, `pageno`, `time_range` (day/month/year), `safesearch`. JSON field names are not in the page I read; from memory they are `results[].{title,url,content}` [U]. Self-hosting is the only sane way; the user must run it.
- DuckDuckGo: no official search API; the ToS names an Acceptable Use Policy I did not read and says nothing on automation [V, https://duckduckgo.com/terms]. Today the HTML endpoint worked [V probe]. The existing scraper depends on HTML class names and "anomaly" detection; a rate-limit page will recur.
- Marginalia [V, https://about.marginalia-search.com/article/api/]: `https://api2.marginalia-search.com/search?query=` with header `API-Key`; `public` key is for testing and "often hits a rate limit"; free non-commercial keys by email under CC-BY-NC-SA; commercial keys metered. Small independent-web index: good for blogs, bad as a general default.

### 3.13 Provider-native and OpenRouter
- Anthropic [V, https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool and .../web-fetch-tool]: `web_search_20250305`/`20260209`/`20260318` and `web_fetch_*` as server tools on `/v1/messages`; **$10 per 1,000 searches** plus tokens; web fetch has no surcharge, supports PDFs, does **not** render JavaScript, and refuses URLs that did not appear in context; ZDR-eligible (dynamic filtering excluded). Not reachable through `/chat/completions`.
- OpenAI [V, https://developers.openai.com/api/docs/pricing]: web search $10/1k calls plus search-content tokens (preview on non-reasoning models $25/1k); `gpt-5-search-api` is the Chat Completions search model ($1.25/$10 per M tokens, no per-call fee listed). No free quota listed.
- OpenRouter [V, https://openrouter.ai/docs/guides/features/plugins/web-search and .../server-tools/web-search]: `:online` = `plugins:[{"id":"web"}]`, **now documented as deprecated** in favour of the `openrouter:web_search` **server tool** (beta) that the model calls 0-N times; `openrouter:web_fetch` likewise. Engines: `auto` (native if the provider has it, else Exa), `native`, `exa`, `firecrawl`, `parallel`, `perplexity`. Engine prices on OpenRouter: Exa $0.007/request (10 results; deep $0.012-0.015), Parallel $0.001 (turbo/fast) or $0.005, Perplexity $0.005, Firecrawl $0.005 + $0.0125/result, native passthrough; all plus tokens for the injected results. Params: `max_results` (default 5), `max_total_results`, `max_uses`, `allowed_domains`/`excluded_domains`, `search_context_size`, `max_characters`, `user_location`. Citations come back as `message.annotations[].url_citation`. Firecrawl is rejected when ZDR is enforced; on `us.openrouter.ai`/`eu.` endpoints most engines are unavailable. Web fetch via Firecrawl costs $0.0025/page.
- Consequence for construct: server tools execute inside OpenRouter, outside construct's tool loop, journal, permission prompts and `cap_output`. Whether the call is visible as a streamed event to `llm/` is [U]. It would be a `dialect.rs` field (provider quirk), not a `Tool`, and it makes `web_search`'s surface model-dependent. At the Exa engine it costs the same $7/1k as calling Exa directly, so the only gain is no key.

---

## 4. Quality: what is measured, by whom

Treat every vendor chart as marketing; they contradict each other. Examples
of that pattern: Parallel's own WISER-Search post (July 2025) shows Parallel
MCP beating native search and Exa MCP [VC, https://parallel.ai/blog/search-api-benchmark];
Perplexity's `search_evals` repo (MIT, run by Perplexity) shows Perplexity first
on 3 of 4 suites and Exa last or near-last on all four, but it benchmarks each
vendor's *agent* product, not its search API [VC, https://github.com/perplexityai/search_evals];
Brave claims SOTA on SimpleQA for its AI Grounding answer product (F1 92.1-94.1%,
Aug-Sep 2025) judged by itself [VC, https://brave.com/blog/ai-grounding/];
Exa publishes a comparison with open code on GitHub where Exa leads people/company
search [VC, per search-result summary of https://exa.ai/versus/brave; page not fetched, [U]].

**Independent or third-party measurements**

| Source | Design | Result | Caveats |
|---|---|---|---|
| arXiv 2607.10198 "Similar Accuracy, Unequal Evidence" (Selvam & Ghosh, v1 2026-07-11, v2 2026-09-13) [V] | Brave vs Tavily (`basic`) vs Firecrawl; 100 of SealQA-Hard; one frozen GPT-5.4 agent; shared Jina Reader fetch | 25 / 25 / 26 correct. Brave exposed gold support before any fetch on 30 queries vs 16/16; Tavily put 50% of such evidence at rank 1 vs 13%/13%; oracle union of the three = 44/100 | one model, one judge (Kimi-K2.6), CIs overlap hugely; Brave returned more snippet text (confound). Arms run sequentially. No vendor funding disclosed |
| arXiv 2506.03011 OpenHands-Versa (2025) [V] | GAIA validation, same agent, three search APIs | Brave 56.96%, Exa 58.18%, Tavily 64.24% | Single run; authors attribute Tavily's lead partly to its LLM-generated per-query answer (a feature, not retrieval quality) and Brave's snippets to raw text; Brave's product has since added LLM Context |
| AIMultiple "Agentic Search" (updated 2026-05-13; Dec 2025 snapshot) [S, https://aimultiple.com/agentic-search] | 100 queries, top-5 results per API at defaults, GPT-5.2 LLM judge, 10% human check, 95% bootstrap CI | Agent score: Brave 14.89, Firecrawl 14.58, Exa 14.39, Parallel Pro 14.21, Tavily 13.67, Parallel Base 13.50, Perplexity 12.96, SerpApi 12.28. Top four indistinguishable; latency Brave 669 ms, Tavily 998 ms, Exa ~1.2 s, Firecrawl 1.3 s, Parallel Pro 13.6 s, Perplexity 11 s+ | Judges result relevance/quality, not task success; queries are AI-topic SEO queries; funder not stated; SerpApi is a disclosed subscriber of the firm's services |
| openbenchmarks.com (last measured 2026-10-21) [S, https://openbenchmarks.com/web-search] | 3 tasks: 300 company-news questions, 100 coding-ticket retrievals, 45 multi-hop; fixed models/judges | Factual: Exa-fast 99.3, Linkup 96.7, Perplexity 97.3, Tavily-adv 93.0, Brave 93.3. Hard retrieval, search-only: Perplexity 77.3, Firecrawl 70.3, Parallel-fast 66.7, Exa-fast 66.3, Tavily-basic 51.0, Brave 38.0. Multi-hop F1 (search+fetch): Exa-deep 48.2, Perplexity 46.6, Parallel 42.3, Tavily-adv 41.0, Brave 29.4 | Operator and funding unstated ("no vendor pays"); site itself warns that vendors benchmarked later score higher and that the factual task "should not be used to compare vendors"; model names (e.g. `gpt-5.6-terra`) I could not verify |

**Reading.** On cheap snippet-level retrieval Brave looks as good as anyone;
when the agent must do hard multi-hop work, the products that return more
extracted text per call (Exa, Perplexity, Parallel, Firecrawl) do better in
the openbenchmarks tables than Brave (which of Brave's endpoints was used is
not stated). That is a hypothesis, not a finding. Vendor-claimed
latencies, independently measured latencies and my one-off probes (0.6-1.0 s
for the two MCP endpoints) are in the provider notes. SimpleQA, FRAMES and
BrowseComp are used by vendors on their own products; I found no independent
FRAMES/SimpleQA comparison of the raw search APIs.

---

## 5. Search plus fetch

### 5.1 What construct does today
`web_search` returns title/URL/<=400 chars; the model then calls `web_fetch`
(plain GET, `Accept: text/markdown, text/html;q=0.9, ...`, htmd, 5 MB cap,
output spilled by `cap_output`). Two model round trips for any page the
snippet cannot answer from, and `web_fetch` fails silently on:
JS-only pages (empty skeleton), bot walls (403/429/503 turned into an
`HTTP <status>` error carrying the challenge page), PDFs (decoded as lossy
UTF-8), and boilerplate (only `script/style/noscript/svg/nav/footer/iframe/form`
skipped).

### 5.2 Providers that fold content into search
| Provider | How | Marginal cost | Notes |
|---|---|---|---|
| Exa | `contents.highlights` (query-relevant excerpts) or `contents.text` with `maxCharacters` | none for first 10 results; summaries +$1/1k pages [V] | adaptive highlights ~2-4k chars/result by default (OpenRouter's description of Exa [VC]); `maxAgeHours` controls live crawl |
| Tavily | `include_raw_content` (`markdown`/`text`) | no documented surcharge [V absence] | `content` is always a chunked summary; raw makes responses large |
| Parallel | `excerpts[]` always on; `max_chars_per_result` | included | compression is their pitch; ranking by objective |
| Perplexity | `snippet` extracted text, `max_tokens_per_page` | included | tunable budget |
| Brave | `extra_snippets` on web search; LLM Context endpoint for chunks | LLM Context price [U] | `maximum_number_of_tokens` default 8,192 |
| Firecrawl | `scrapeOptions` markdown | +1 credit/page, +1/PDF page | the only one that also does anti-bot and PDF in the same call |
| Kagi | `/search` can return full markdown of top results [V] | [U] | |
| Jina | `s.jina.ai` returns top 5 with content | >= 10k tokens/request [VC] | needs key |

Compared with the two-step flow: a combined call removes one round trip for
the common "read the top result" case and lets the provider choose the
relevant passage (highlights/excerpts are query-aware, which a fetch plus
truncation is not). It costs more tokens per search call even when the model
needs only titles. Neutral design: keep `web_search` returning excerpts of
~1,000-1,500 chars for the top few, and keep `web_fetch` for full pages.

### 5.3 Fixing `web_fetch`
| Weakness | Local, free | Remote option (cost) |
|---|---|---|
| Sites that serve markdown | Already sent `Accept: text/markdown`; a `text/markdown` body passes through unchanged. Probe of 12 pages: 8 returned `text/markdown` (Cloudflare docs and blog, Anthropic docs, OpenRouter docs, Vercel docs, Stripe docs, Exa docs, Tavily docs) and 4 HTML (docs.rs, Wikipedia, GitHub repo page, Hacker News) [V probe, n=12, not a sample]. Cloudflare's "Markdown for Agents" is opt-in beta on Pro/Business/Enterprise zones and returns `x-markdown-tokens` [V, https://blog.cloudflare.com/markdown-for-agents/] | n/a |
| Boilerplate | Readability extraction: `dom_smoothie` 0.19.0 (Readability.js port, published 2026-10-27, ~817k downloads) [V crates.io] pulls `dom_query`, `tendril`, `gjson`, `phf` and others; `html5ever` is already in `Cargo.lock` via htmd. Adds deps (convention 8: it must remove more code than it adds; it would not, unless a bench shows boilerplate hurts) | Jina Reader, Firecrawl, Exa/Tavily/Parallel extract |
| JS-rendered pages | none in-process | Jina Reader (headless by default, keyless 20 RPM) [VC], Firecrawl, Linkup `renderJs` ($5/1k), Exa `/contents` with `maxAgeHours:0` [V param], Parallel `fetch_policy` |
| Bot blocking (403/429/503, Cloudflare challenge) | one retry with a browser-like UA is the most you can honestly do locally | Firecrawl (enhanced proxy, no extra credit [V]), Jina, Exa/Parallel/Tavily cached copies |
| PDFs | none (a Rust PDF text crate is a large dep) | Firecrawl 1 credit/PDF page; Jina native; Anthropic native fetch returns base64 PDF; Tavily [U] |

Standalone extract prices [V]: Exa `/contents` $1/1k pages per content type;
Parallel `/extract` $1/1k URLs; Tavily `/extract` 1 credit per 5 URLs
(= $0.0016/URL at $0.008/credit; advanced $0.0032); Linkup fetch $1/1k
(no JS) or $5/1k (JS); Kagi `/extract` $4/1k; Firecrawl 1 credit/page
(~$1/1k on Standard); Perplexity Agent API `fetch_url` $0.50/1k;
Jina Reader token-metered, keyless 20 RPM. Privacy note: every remote fetcher
learns which URL you read; a local GET only tells the site.

---

## 6. Integration design for construct

### 6.1 Shape
Keep the existing pattern: one `async fn` per backend returning `Vec<Hit>`,
all `reqwest` + `serde_json`, no new dependency for any of Exa, Brave,
Tavily, Parallel, Perplexity, Linkup, Kagi, Mojeek, SearXNG. `Hit` grows
from `(title, url, snippet)` to a small struct with `date: Option<String>`.
Per AGENTS.md rule 5, wire strings (secret names, `"exa"` etc.) are declared
once. Secret names to add to `crates/construct/src/secrets.rs` known keys:
`TAVILY_API_KEY` (and later others).

### 6.2 Request/response mapping
| Backend | Request (tool args -> API) | Hit mapping |
|---|---|---|
| Exa | `POST https://api.exa.ai/search`, `x-api-key`; `{query, numResults, contents:{highlights:{maxCharacters:N}}, includeDomains, startPublishedDate}` (drop `numSentences`); `type` from config (`auto` default, `lite` at $4/1k) | `title`, `url`, `publishedDate`, `highlights[]` joined |
| Brave | `GET /res/v1/web/search`; `q`, `count`, `freshness=pd|pw|pm|py`, `extra_snippets=true`; domains: prefix `site:` terms to `q` (one domain) | `title`, `url`, `description` + `extra_snippets`, strip `<strong>` |
| Tavily | `POST https://api.tavily.com/search`, Bearer; `{query, max_results, search_depth:"basic", time_range, include_domains}` | `title`, `url`, `content`, `published_date` |
| Parallel | `POST https://api.parallel.ai/v1/search`, `x-api-key`; `{objective: q, search_queries:[q], mode:"fast"|"basic", advanced_settings:{max_results, source_policy:{include_domains, after_date}, excerpt_settings:{max_chars_per_result:N}}}` | `title`, `url`, `publish_date`, `excerpts[]` joined |
| Perplexity | `POST https://api.perplexity.ai/search`, Bearer; `{query, max_results, max_tokens_per_page, search_domain_filter}` | `title`, `url`, `snippet`, `date` |
| SearXNG | `GET {base}/search?q=&format=json&time_range=` | `results[].title,url,content` [U field names] |

Each backend maps `recency` to its native filter (`startPublishedDate`
computed from now; `freshness`; `time_range`; `after_date`). Where a backend
cannot (SearXNG partly, DDG not at all), ignore it and say so in the output
line rather than erroring.

### 6.3 Tool schema: what to expose
Today: `query`, `limit`. Add **only** `recency` (enum `day|week|month|year`,
the highest-value filter for "latest version/CVE/news" queries, which is what
`Q_WEB` in the router is about) and `domains` (array of include hosts, for
"search the docs site"). Both are supported by Exa, Brave (via `site:` for
one host), Tavily, Parallel, Perplexity, Linkup, Firecrawl. Keep **provider-specific
knobs out of the schema** and in config (`[web]`): Exa `type`
(`auto|lite|deep-lite`), Exa/Tavily `category`/`topic`, Tavily `search_depth`,
snippet length. Neural-versus-keyword is Exa-internal in `auto`; letting the
model choose burns tokens in the first request ("keep the first request
small"). Exclude-domains is not worth a parameter. Record the added schema
bytes against the golden request snapshots; `cargo xtask snapshots` will show
the diff, and the routed-web snapshot changes by design.

### 6.4 Snippet size
400 chars is the thinnest on the market: OpenRouter's Exa default is ~2-4k
chars/result, Parallel's default 1,500, Brave LLM Context 8k tokens total
(all [VC]/docs). Suggest `[web] snippet_chars = 1200` for the first 5
results and 300 for the rest; 8 results is then about 2.5k tokens. Print
`date` when the backend supplies it. Let `cap_output` handle spills as it
does for fetch. Measure with a bench arm before settling.

### 6.5 Provider selection
- Explicit beats implicit: `[web] search = "auto"` (default) or a name;
  `CONSTRUCT_WEB_SEARCH` env override for one run, like `CONSTRUCT_TOOLS_EDITOR`.
- `auto` = first key found in a documented order, matching today's behaviour
  (Exa, Brave) and appending Tavily; the order is the user's preference list,
  not a ranking claim, since the evidence above does not rank them.
- Fall-through on failure only for *errors that say nothing about the query*
  (network, 5xx, 429 after one retry), not 401/402/403/432/433, so a spent
  quota or bad key is shown to the user instead of silently using another
  vendor with a different privacy posture. Say which backend answered in the
  output header line.
- Keyless: `auto` ends with, in order, a configured SearXNG base URL if set,
  then the vendors' anonymous endpoints if the user opts in
  (`[web] anonymous = true`, privacy: queries go to Exa or Parallel), then DDG.
  I would make the anonymous MCP step opt-in because it sends queries to a
  third party under no account; DDG already does the same silently, so the
  honest default is to keep DDG and document the trade.

### 6.6 MCP versus direct HTTP
ADR 0011: the MCP client is stdio-only and HTTP transports are "the next
addition", so hosted MCP servers (Exa, Parallel, Tavily, Brave, Perplexity)
cannot be listed as `[mcp.*]` today. The local stdio route exists for Exa
(`exa-mcp-server` npm package with `EXA_API_KEY`, [V per exa-mcp docs]) but
drags in Node. A direct HTTP call is smaller than MCP plumbing, journals as a
normal tool, and keeps `web_search`'s name and output stable, so the router and
`Group::Web` need no change. MCP remains right for providers' richer tools
(Exa `agent_run`, Parallel Task) which are out of scope for a search tool.

### 6.7 Errors and rate limits
Current gap [V by reading `web.rs`]: `exa()` and `brave()` call
`.json()` without checking `status()`, so 401, 402, 429 and 5xx turn into
"no results" or a JSON-parse error. Minimum: check status; on 429 read
`Retry-After` (Tavily documents it, integer seconds) and retry once with a
cap of ~5 s; on 401/403 say "<NAME> rejected the key"; on Tavily 432/433 say
"plan limit exceeded"; on Brave/Exa surface the body's `error`/`message`
field. Keep the `Result<String, String>` prose-for-the-model convention
(AGENTS.md rule 4). Respect Brave's 1-second window by not retrying in less
than 1 s; construct issues serial calls so per-key QPS (Brave 50, Perplexity 50)
is not a concern. Tavily dev keys are 100 RPM [V].

### 6.8 Secrets, ToS, privacy
- Keys resolve through `crate::secrets::get` in the parent process; none enter
  the shell sandbox (scrubbed env). Nothing changes for that.
- Every query goes to the chosen vendor. Models write queries from repository
  context, so a query may contain internal names or error strings. Vendor
  retention is mostly unpublished: Exa ZDR is Enterprise-only [V]; Brave ZDR
  Enterprise [V]; Firecrawl ZDR 5x credits [V]; Mojeek 90 days, ZDR on request
  [V]; Tavily may use queries to improve responses and share with index
  providers [V]; Parallel, Perplexity, Linkup, You.com, Kagi: not established
  [U]. Document this next to the key description and make the output header
  name the backend (6.5).
- Storage rights: Brave's plan tiers distinguish "storage rights" [V, unlisted
  which]; Mojeek Startup allows 1-hour caching [V]. construct's journal stores
  tool results; if that counts as storage, check the plan before choosing
  Brave or Mojeek for it. Not analysed legally.
- ToS: scraping a UI endpoint (DDG) or Google (SERP resellers) is the only
  class with live legal friction [S: Google v. SerpApi]; official APIs are fine
  for programmatic use. The anonymous MCP endpoints are offered as free by their
  vendors [V]; their written terms I did not read [U].
- Prompt-injection: search excerpts are untrusted text just like fetch output;
  no change from today, but combined calls place more of it in context.

### 6.9 First-request budget
Router-gated: `web_search`/`web_fetch` are in `Group::Web`, enabled per turn.
Adding two optional properties adds roughly 150-250 bytes to that group's
schema; the plain-turn snapshot is unaffected. A `provider` argument or the
OpenRouter server tool would change the schema or the body; avoid both.

---

## 7. Probes run (keyless, one request each unless noted)

| Probe | Result |
|---|---|
| `POST html.duckduckgo.com/html/` `q=rust tokio select` | HTTP 200, 28.8 KB, 10 `result__a` anchors |
| `GET r.jina.ai/https://example.com` | 200, markdown with `Title:` / `URL Source:` header, `x-usage-tokens: 341` |
| `GET s.jina.ai/?q=rust` | 401 |
| `POST https://mcp.exa.ai/mcp` initialize then `tools/call web_search_exa {query, numResults:3}` | 200 SSE, 0.97 s, highlights text returned; session id issued (whether it is required: [U]) |
| `POST https://search.parallel.ai/mcp` initialize then `tools/call web_search {objective, search_queries}` | 200 JSON, 0.60 s, `results[].{url,title,publish_date,excerpts[]}` |
| 6 public SearXNG instances, `/search?q=...&format=json` | 4 x 429, 2 x 200 text/html (no JSON) |
| `Accept: text/markdown` against 12 pages | 8 `text/markdown`, 4 `text/html` (section 5.3) |
| `crates.io` API for `dom_smoothie`, `htmd` | 0.19.0 (2026-10-27), 0.5.6 (2026-10-15) |
Search queries were innocuous public technical strings. No authenticated
calls, no sign-ups.

---

## 8. Open questions and unverified items

1. **Authenticated behaviour.** Real response shapes, 429/401 bodies, and
   latencies for Exa, Brave, Tavily, Parallel, Perplexity were not exercised.
   A tier-1/2 bench run with keys would settle shapes and cost
   (`costDollars` in Exa responses, `usage.credits` in Tavily).
2. **Exa `lite` vs `auto` quality.** `lite` ($4) supports highlights, domains,
   dates and category, so it could be the cost default; no evidence ranks it.
3. **Brave LLM Context** price/plan, and whether it beats the plain web
   endpoint plus `extra_snippets` for agents. Brave's free allowance ($5 vs
   "up to $20") and which plan carries storage rights.
4. **Parallel and Perplexity free tiers**, Parallel rate limits, Perplexity
   recency filter, Sonar prices, Serper paid prices, Kagi account requirements.
5. **Anonymous MCP endpoints:** limits, written ToS, whether a session id is
   mandatory, stability. They are the most interesting keyless idea and the
   least documented.
6. **OpenRouter server tools through `/chat/completions`:** doc examples were
   tabbed code I could not see; how tool calls/results appear in the stream
   and whether a client-side tool loop can coexist with `openrouter:web_search`
   is untested.
7. **Quality ranking:** independent studies are small (100 queries each),
   single-judge, and disagree on order. A construct-specific bench task
   (real model, search arms) is the defensible way to choose, and fits
   AGENTS.md "A/B a construct setting with arms".
8. **Tavily `/extract`** JS and PDF capability, Exa `/contents` PDF handling,
   and Jina's current terms after the Elastic change.
9. **Legal:** DuckDuckGo Acceptable Use Policy; Google v. SerpApi outcome;
   whether construct's journal counts as "storage" under Brave/Mojeek terms.
10. **Readability crate:** compile-time and binary-size cost of `dom_smoothie`
    not measured; only worth it if a fetch bench shows boilerplate noise costs
    accuracy.

If this becomes a decision, the ADR candidates are: (1) provider selection and
failure semantics (6.5, 6.7), (2) whether anonymous third-party endpoints may
be a default, (3) whether server-side search (OpenRouter/native) is ever a
`dialect` feature.
