# Compaction features: merged list

Source: `docs/proposals/10-*` and `11-*`, checked against the code on `master`
(`origin/master` fetched 2026-10-21, no newer commits). Status comes from the
code, not the docs' status lines. Journal recall (`recall_result`,
`recall_turns`, proposal 10 K) is left out on purpose. Anything undecided,
exploratory or "measure first" is dropped (see the end).

Model: the journal is the record, the model's context is a view built from it,
and compaction only changes the view. Every cut is a journaled
`context_compacted` event, so replay can rebuild any request.

## Baseline (existed before the proposals; `src/core/context.ts`)

- **Trigger and target.** Cut when context passes `COMPACTION_THRESHOLD` = 0.7
  of the working set; a cut must bring it to `COMPACTION_TARGET` = 0.5.
  The working set is `min(contextWindow, cap)`.
- **Planner (`planCompaction`).** At the trigger, try candidate A: elide
  results down to `KEEP_RESULTS` = 2 turns. Take it if it reaches the target.
  Else candidate B: summarize up to the turn before `KEEP_TURNS` = 4. Else
  elide if that at least advances the cut, or do nothing. A summary that would
  not reach the target is refused, and the run sends the oversized request
  with a once-per-run diag.
- **Aging.** Independent of size, a result older than `AGE_TURNS` = 16
  assistant turns is elided each round, one turn at a time. This keeps the
  provider's prompt cache warm, because the cut moves slowly.
- **Stubs.** An elided result becomes a stub with a one-line gloss. The
  summarizer writes the glosses (`## Stubs`).
- **Summary source.** Written from the original journal events, with seq
  citations (`src/prompts/summarizer.md`: Goal / Decisions / Files / Open /
  Anchors). It builds on the previous summary only when the originals exceed
  `SUMMARY_INPUT_FRACTION` = 0.6 of the summarizer's window. That is recorded
  as mode `summarize-incremental`.
- **Per-result cap.** `RESULT_FRACTION` = 5% of the window, at most 48 KB.
- **Summarizer model.** `resolveSummarizer` takes the settings summarizer, then
  a default (`DEFAULT_SUMMARIZER_MODEL_ID`), then the run's model.
- **Compaction blocks the run.** It runs between rounds.

## Shipped since

1. **Overflow recovery** (10 A). `contextOverflowError`
   (`src/model/retry.ts`) uses pi-ai's `isContextOverflow`. On a provider
   "too long" rejection, force the cut the planner would make at the
   threshold, journal it with `reason: 'overflow'`, and resend once. A second
   rejection, or a cut that writes nothing, fails the run.
2. **Size the next request** (10 B). The trigger reads the provider's last
   count plus `pendingTokens`: the last reply, its results and anything after
   it, estimated on the projection. The event keeps `inputTokens` as the
   provider's count and adds `pendingTokens`.
3. **Measured chars per token** (10 C). Each `model_request` journals
   `inputChars` and `reasoningTokens`. The reducer pairs them with the
   response's count and keeps `state.measured` (ratio, model, seq). The
   planner uses the most recent value, only for the same model, and falls
   back to the `run.charsPerToken` setting (default 4). Requests under 1,024
   tokens don't measure; the ratio is clamped to 1–6. The ratio feeds only the
   planner (trigger, `pendingTokens`, target check, `estimatedAfter`), never
   tool result windows. `context_compacted` records the `charsPerToken` used.
4. **`elapsedMs` on every `context_compacted`**, to measure how long
   compaction blocks a run.
5. **Request overhead counted** (found by the e2e run). The system turn and
   tool schemas (about 3.4k tokens) are in the provider's count, so the
   target check and `estimatedAfter` now include them
   (`requestOverheadTokens`, `overheadTokens` on the event). Without this a
   cut at 32k landed near 60% while the planner believed 50%.
6. **Warm summarizer** (11 A), setting `compaction.request`: `'cold'`
   (default) or `'warm'`.
   - Warm sends `[system turn] [tools] [the projection] [user: compaction task]`
     with `toolChoice: 'none'` on the run's own provider, so the prefix reads
     from cache. The task (`warmSummaryTask`) names the cut, lists removed
     turns by seq, carries the `summarizer.md` text and asks for `## Stubs`.
   - It uses the projection, not the originals. No originals are added back.
   - A warm summary over an existing summary is recorded as
     `summarize-incremental`.
   - Falls back to cold, with `requestFallback` saying why, when the
     summarizer isn't the run's model, when the cut was forced by overflow, or
     when the warm request fails or returns no summary.
   - The event records `request: 'cold' | 'warm'` and, for warm, `continued`
     (seq of the run's last `model_request`).
7. **Tests and verification.** `verify:compaction` (and `-scenarios`, `-live`)
   run at a 32k window with honestly counted fixtures. The `steady` scenario
   checks that each cut's `estimatedAfter` matches what the provider then
   sees. `npm run replay:context` rebuilds every request's input;
   `npm run measure:tokens` measures chars per token offline.

## Specified, not built

Concrete enough to build. Ordered as in the proposal.

- **G. Policy parameters as settings.** Move `COMPACTION_THRESHOLD`,
  `COMPACTION_TARGET`, `KEEP_TURNS`, `KEEP_RESULTS`, `AGE_TURNS`,
  `RESULT_FRACTION` and the soft line (I) into a `compaction.policy` settings
  object, with today's values as defaults and a stable policy id. Record the
  id and every parameter in the run manifest, and print the policy per request
  in `replay:context`.
- **H. Scorecard over recorded journals** (`measure:compaction` or
  `replay:context --summary`). One row per run: rounds; peak
  `contextTokens / workingSet`; cuts by mode (aging, elide, summarize,
  summarize-incremental); summarizer tokens and cost share; cache-read ratio
  per request and in the request after each cut; overflow and "cannot compact
  further" diags; outcome. All of it is already journaled. (Its recall-call
  column is out of scope here.)
- **I. Soft and hard lines; prefer a cut over none.**
  - A soft line (default 0.6) below which nothing size-driven happens; above
    it, aging advances one extra turn per round.
  - The hard line (default 0.7) is the planner as today.
  - When neither candidate reaches the target, take the one that lands
    lowest, and allow a re-summary at most once per N rounds instead of
    refusing. The event records why.
- **J. Recent window as a share of the working set, floored by turns.** Keep
  whole turns, as many as fit in `keepFraction` × working set, never fewer
  than `KEEP_TURNS`. The default reproduces today's behavior at a 128k working
  set. The unit stays the turn, so a call is never split from its result.
- **Output reservation.** Plan against the working set less the run's
  `maxTokens`, and record in `run_started` which. The code has none today
  (`budgetFor` doesn't subtract it), even though the proposal's session list
  marks B "with the output reserved" as shipped. Treat this as open.
- **Replay of the summarizer's request** (warm or cold). `replay:context`
  doesn't rebuild it, because the tools a request carried aren't journaled.
  Journal them first.

## Dropped

- Journal recall and its removal (10 K): excluded at your request.
- Background summarization (10 D): "measure before deciding".
- Manual compaction with instructions (10 E): open on who may call it.
- Handoff/reset (10 F): exploratory.
- Summary tree as a second policy (11 B) and `zoom`: proposed, gated on an
  offline comparison.
- Warm summarizer `auto` mode and re-adding originals to a warm request
  (11 A options): undecided, wait for cost data.
- Pi Durable's 0.6 / 0.85 / 0.3 numbers (not swept), token-only keep windows
  (failed in Pi #10583), and building every summary on the previous summary.

## Docs that no longer match the code

- 10's header says D, E and F are open and A, B, C shipped, which matches.
  Its "Suggested sessions" line 3 claims the output is reserved, which it
  isn't.
- 10's "Current state" still lists overflow-is-fatal, fixed chars per token
  and no pending sizing. All three are now shipped.
- 11's "Not yet" list is accurate.
