# Jev tool routing in coding agents

Research note, 2026-11-17. The examples below come from public source code
and published project evaluations. I inspected their implementations and
reports without running live Jev calls or reproducing their benchmarks.

## What Construct already measured

Strict yes-or-no probabilities, called nouls, matched the expected group
set on 43/47 tuned and 16/18 held-out requests. The two-choice design
scored 40/47 and 15/18. Loose wording such as whether a tool might help scored
12/47 on the tuned set. The separate release-signing benchmark used the older
choice router and measured one task. It found 6/6 correct skill suggestions and
17 median model requests with Jev versus 28 with local routing. The labeled
routing cases measured group and skill selection. Mid-turn discovery remains
unmeasured.
[ADR 0003](../adr/0003-strict-nouls-for-jev-routing.md),
[labeled evaluation](../../bench/jev/eval.py),
[release-signing benchmark](../../bench/bench.py#L1194).

Construct routes before the first request of each user turn. Jev sees the
request, a short slice of the previous assistant reply, skill descriptions,
and MCP server descriptions. The agent keeps groups enabled once added.
`enable_tools` accepts group names, and calling a hidden tool by its exact
name already enables its group. That leaves a discovery gap when the agent
has a specific current goal and knows neither the tool nor the right group.
One MCP server can also stand for many tools, so correctly identifying the
server can still load more schemas than the agent needs.
[Routing input](../../crates/construct/src/agent.rs#L130),
[turn setup](../../crates/construct/src/agent/user_turn.rs#L53),
[hidden-tool recovery](../../crates/construct/src/agent/tools.rs#L92),
[catalog](../../crates/construct/src/tools/catalog.rs#L95).

## Public implementations

| Project | What the code does |
|---|---|
| [burnigtm/jev-mcp](https://github.com/burnigtm/jev-mcp/blob/521d76d01183f2230ec84ec5834413cb408ca85a/src/tools/tool-route.ts#L170-L243) | Takes a task, a current observation, and host-prepared candidates. It filters ineligible candidates locally and sends purpose, effect, and argument shape to Jev. One request asks for a choice with a `none` option and one independent suitability noul per candidate. Low or incomplete evidence hands control back to the caller. [Question pack](https://github.com/burnigtm/jev-mcp/blob/521d76d01183f2230ec84ec5834413cb408ca85a/src/packs/tool-route.ts#L3-L19) |
| [DoGMaTiiC/hermes-jev](https://github.com/DoGMaTiiC/hermes-jev/blob/5dddbeaac5be08281e6c3b372eeaba3639971f0a/plugins/jev-skill-router/jev_router/router.py#L207-L270) | Ranks the skill roster by metadata, then rereads excerpts from a shortlist. A separate fit question can reject the candidate even if it won the ranking. The hook puts one skill hint in the model's context or stays silent. [Hook](https://github.com/DoGMaTiiC/hermes-jev/blob/5dddbeaac5be08281e6c3b372eeaba3639971f0a/plugins/jev-skill-router/__init__.py#L203-L226) |
| [himomohi/jev-skill-router](https://github.com/himomohi/jev-skill-router/blob/b3be6ec9545fc2526abaa8be135fbe0bd34b78e9/src/jev_skill_router/router.py#L99-L116) | Offers an agent-callable MCP skill router with optional local retrieval. It exposes the difference between an empty lexical shortlist and a semantic rejection. The judge ranks and checks candidates before the tool returns selected instructions. [Ranking and fit checks](https://github.com/himomohi/jev-skill-router/blob/b3be6ec9545fc2526abaa8be135fbe0bd34b78e9/src/jev_skill_router/router.py#L157-L220) |
| [legacybridge-tech/pi-typesafe-jev](https://github.com/legacybridge-tech/pi-typesafe-jev/blob/17e66fcd6ecfbb4c3083f748af510bf59cda985f/src/index.ts#L576-L671) | Lets the agent explicitly call Jev. A batch can contain independent choices, nouls, and scores about the same state. A separate tool loads question definitions from a file so the model can supply evidence against a fixed rubric. |
| [thruwire/foreman](https://github.com/thruwire/foreman/blob/c3142ef636617f32f772140125aa310d81ceece9/src/foreman/routing.py#L263-L306) | The source of Construct's strict wording now routes a hierarchy of responsibilities. It checks child groups only after selecting their parent and uses separate thresholds per route. This is relevant if the capability catalog grows substantially. [Thresholds](https://github.com/thruwire/foreman/blob/c3142ef636617f32f772140125aa310d81ceece9/src/foreman/routing.py#L389-L438) |

Hermes itself offers a local discovery baseline. Its opt-in Tool Search gives
the agent separate search, describe, and call tools for deferred tool schemas.
It scales the visible listing from names and descriptions down to server
summaries as the catalog grows. This puts the retrieval step where the agent
knows what it needs. A Construct experiment can compare Jev against a local
search through the same interface.
[Hermes Tool Search documentation](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/tool-search.md).

## What their measurements tell us

- TypeSafe's skill cookbook reports 452 requests using Haiku 4.5, Jev 1.14.0,
  and a 194-skill Hermes roster. With a two-pass hint, wrong skill loads
  fell from 15.7% to 7.9%, and unnecessary loads fell from 14.0% to 5.2%.
  The agent saw a 60-character index per skill. The second pass had longer
  descriptions and excerpts. These are skill-selection outcomes on that
  setup. [TypeSafe cookbook and evaluation](https://docs.typesafe.ai/cookbooks/skill_suggestion).
- In a Claude Code shadow run, 28 of 539 suggestions preceded a call to
  that skill in the same session within 30 minutes. The hook logged suggestions
  privately, so the result measures overlap with the agent's later choice.
  The author's sample of unused suggestions included
  many off-target picks from subagent traffic.
  [Shadow study](https://github.com/shimo4228/jev-skill-router/blob/ad3b942b2b332cd3365698cb05bd88d5a53818ce/README.md#L19-L33).
- In PyModel's 150-question Pi study, the model made zero optional Jev tool
  calls. Forcing the call added 10.4 seconds of median paired wall time.
  The added wall time exceeded Jev's own round-trip time. Accuracy was
  unmeasured.
  [Agent study](https://github.com/PyModel/jev-judge-mcp/blob/358b684281d5216298f7d7e8a6e1d849e8bbe4ea/README.md#L302-L319).
- The `jev-gateway` benchmark attempted 120 coding-agent sessions on
  two chess-engine tasks and excluded one contaminated run. Time, cost,
  and completion varied by agent and task. One feature arm fell from
  5/5 solved to 3/5. The gateway later
  stopped vetoing Codex tool calls on a confident no-tool prediction by
  default. In the author's local logs, Codex chose to call a tool after 36/52 such
  predictions. That is disagreement, with tool usefulness unmeasured.
  [Benchmark and version caveats](https://github.com/vinilana/jev-gateway/blob/83b5b98279f7b932db7f03388d9e95a873025063/README.md#L741-L778),
  [Benchmark exclusions](https://github.com/vinilana/jev-gateway-bench#cross-run-contamination),
  [Codex default change](https://github.com/vinilana/jev-gateway/blob/83b5b98279f7b932db7f03388d9e95a873025063/README.md#L601-L610).
- `jev-gateway` can collect a proposed next tool alongside an unmodified model
  request. The upstream request starts before the Jev call. `mu` also tracks
  whether each decision differed from its default and, where observable,
  what the agent did afterward.
  [Gateway shadow mode](https://github.com/vinilana/jev-gateway/blob/83b5b98279f7b932db7f03388d9e95a873025063/README.md#L158-L181),
  [mu decision logs](https://github.com/qybaihe/mu/blob/5d8b1d13a154bd5b68bb7466808050dc193f3f62/docs/judges.md#L206-L229).

## The first Construct experiment

Test whether Jev can surface a missing capability **after the agent has
learned something about the task**. Start with a shadow decision at a
selected tool-round boundary, such as an empty local lookup that leaves a
specific question unresolved. Give the router the immediate goal, the
type and outcome of the last action, and a small list of eligible hidden
capabilities. Send descriptions and short outcome summaries. For example,
a local search for an undocumented config setting may return no matches,
and a configured docs server may be the next useful capability.

Use local retrieval to form the shortlist and ask independent fit nouls in
one Jev request. All candidates can fail the fit check. Compare local
retrieval alone with the same shortlist reranked by Jev. For the first
advisory arm, put one hint in the next ordinary request and let the agent
choose through `enable_tools`. An on-demand goal query can use the same
shortlist if the experiment shows occasions where the model recognizes
that it needs help. Consider cached per-tool metadata when measurements
show that activating an entire MCP server is materially expensive.
[Construct's model-driven switch](../../crates/construct/src/agent/tools.rs#L20),
[TypeSafe probability routing](https://docs.typesafe.ai/patterns/probability-routing).

Use the existing journal to join each prediction to the triggering
`ToolEnd`, the next `Request`, and eventual tool use. In shadow mode,
manually label whether the proposed capability would have helped. Interleave
the current router, a local shortlist hint, and a Jev-reranked hint. Score
task success, time to first useful call, unnecessary activations, request
bytes, cache hits, and total wall time. Include a task where a missing
capability becomes apparent mid-turn, a similar task that can finish locally, and a
task that needs two capabilities. Re-run both existing routing sets
before and after, and record the resolved Jev version. The model's later
tool choice is a diagnostic signal. The task checker supplies the outcome.
[Journal event types](../../crates/construct/src/event.rs#L66),
[existing routing scorer](../../bench/jev/eval.py),
[existing end-to-end routing benchmark](../../bench/bench.py#L1194).
