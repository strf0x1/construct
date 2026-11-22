# 0063 — A growing tool list misses the prompt cache; `[tools] initial` to measure it

Status: accepted. 2026-11-21.

## Context

ADR 0005 keeps tool groups on once enabled so the `tools` array never
shrinks or reorders, and says "provider prefix caches stay warm after the
first enable; later turns pay only for what grew". That is not what happens.
Providers put the tool definitions ahead of the system prompt and messages
in the cached prefix, and a group switched on is inserted into the middle of
the array (catalog order, before `enable_tools`, whose description changes
too). Any growth changes the prefix from its first tool on, so the request
after every enable misses the cache entirely, not only for what grew.

Issue #187 asked which things change ahead of the cache breakpoint between
requests. A two-turn TUI session against `construct-dev serve --cache`
(scripted model, simulated prefix cache) that reads, enables `web`, calls
`memory`, starts a background shell, writes and answers, then takes a second
turn, found exactly three breaks, all from the tool list growing: `web`
enabled, `memory` switched on by the call, `job` offered after the
background shell. The system prompt and the message history were
byte-stable throughout, across the memory edit, the job wake and the turn
boundary.

## Decision

Nothing changes by default. A new setting, `[tools] initial`, lists groups
that are on from the first request (`web`, `memory`, `history`, `skills`,
`developer`, `jobs`, ...); a label that is not a built-in group fails the
agent build, so a mistyped bench arm cannot silently measure the default.
MCP groups are not accepted there: their servers start when enabled.

`bench/journal.py cache` counts a drop as a call whose cached tokens are
under half the previous call's prompt on the same model (the prefix it
re-sent was mostly not reused), instead of any fall in cached share, which
flagged calls that only added much new content and missed consecutive
misses.

## Consequences

- The cost of routing is now measurable as an arm:
  `bench.py task --arm routed:'' --arm initial:'initial=["web","memory","jobs"]'`
  on a caching provider, judged with `bench.py compare --only '^prompt_cache\.'`
  together with cost and success.
- ADR 0005's rationale for "only grow" still holds against shrinking, but
  its claim that growth is cheap is wrong. Whether the default should send
  more groups up front is a trade against AGENTS.md's "keep the first
  request small", which matters for local models with small windows and no
  cache, and for tool-choice accuracy. That decision waits for the tier 2
  numbers and becomes its own ADR.
- A tier 2 run confirmed the miss on a real provider (Evidence). Single-turn
  tasks rarely show it: the router turns a group on before the first request,
  so the list only grows on a later turn, on an `enable_tools` call, on the
  first `memory` call or after a background job. `bench/tasks/cache-grow`
  does all of that across three turns, to measure it.
- A second break, not from tools: Anthropic drops an earlier turn's thinking
  blocks once a new user turn starts, so the first request of the turn after
  one with reasoning misses too, in both arms. construct sends the same bytes
  (the mock shows the prefix stable there); it is a provider rule, outside
  this decision.

## Evidence

Tier 1, deterministic, `construct-dev serve --cache`, the probe session
above (9 requests), read back with `bench/journal.py cache`:

| | default (routed) | `initial = ["web", "memory", "jobs"]` |
|---|---|---|
| input tokens | 17,341 | 18,903 (+9%) |
| cached share | 58.0% | 86.4% |
| uncached input tokens | 7,275 | 2,578 (−65%) |
| calls with 0% cached after call 1 | 3 (calls 3, 4, 5) | 0 |

The simulated cache is a byte-prefix match over the request JSON, which is
the behaviour this ADR claims for real providers but is not proof of it.
`crates/construct-dev/tests/e2e.rs`
(`tools_initial_keeps_the_tool_list_and_the_prompt_cache_stable`) pins both
halves: the request after `enable_tools` gets 0 cached tokens by default,
and with `initial` the tool list never changes and every later request hits.

Tier 2, `anthropic/claude-haiku-5.5` on OpenRouter, `bench/tasks/cache-grow`
(three turns: code fix, something to remember, a background job), 5 rounds
per arm, `bench/results/task-20261121-192153.json`, judged with
`bench.py compare --a routed --b initial`:

| | routed | `initial = ["memory", "jobs"]` |
|---|---|---|
| calls right after the tool list grew | 12, every one 0 cached | none |
| cached share (median) | 58.3% | 77.6% |
| drops (median per run) | 3 | 1 (the thinking break above) |
| first request prompt tokens | 2,094 | 2,599 (+505) |
| cost per run (median) | $0.00385 | $0.00276 (−28%) |
| success | 5/5 | 5/5 |

The same model on six single-turn tasks (`task-20261121-182005.json`, 3
rounds) never grew the list mid-session in either arm: no drops, the same
success, and `initial = ["web", "memory", "jobs"]` cost 8% more for its
larger first request. Where nothing is switched on later, sending groups up
front only costs.

Related: ADR 0005, ADR 0012, issue #187.
