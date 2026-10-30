---
id: status-line
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/status.rs, crates/construct/src/journal/totals.rs, crates/construct/src/journal/show.rs]
verified_at: 066a40d
---
# Status line

The bottom row of the TUI shows, left to right: a lavender `◆` and the current model, then on the right any toast or learning notice, the enabled tool groups, the counters `ctx ▰▰▱▱▱ 37% · billed ↑487k ↓6.2k · cache 20% · $0.123`, a scroll marker (`scrolled N`) when scrolled up, and the short session id. `ctx` is how full the window is now (from the newest request; `<1%` for a tiny prompt, `~37%` for an OpenRouter router like `openrouter/free`, whose window is a guess); its meter is blue, soft amber from 50%, amber from 70%, red from 90%. `billed` is every token sent (↑) and received (↓) this session, compaction summaries and delegated sub-agents included, so it grows well past the window. Cache turns cyan as the hit rate rises; cost (provider-reported, else the catalog price) is muted green. Labels are faint, numbers soft gray. A resumed session starts from what it already spent. The counters and id are never trimmed: at narrow widths the toast is cut first, then the groups, then the model name. `/status` draws a rounded `status` box (session id, journal path, model, context, requests, input/cached/output/cost, summed from the journal, plus a `side calls` row for what summaries and sub-agents took) and `/copy status` copies it.

## Sub-features

- `status-model` the current model next to a lavender `◆`.
- `status-tokens` the session's token total, updated after each request.
- `status-reset` `/new` sets the total back to zero.

## How to get to it (user POV)

- Always visible at the bottom of the TUI; it updates as a turn runs.

## Done when

- After a turn, the bottom row shows the model, the `ctx` meter and a non-zero `billed` count; `/new` resets the count.

## Driving it

Preconditions:

- Baseline state. Model: one text reply `hello from the mock`. Recipe: `.agents/skills/verify-construct/recipes/status-line`.

- **Turn.** Drive: `type say hi`, `key enter`, `wait "hello from the mock"`, `settle 300 5000`. Expect: the reply.
- **Look.** Drive: `expect "billed ↑"`, `expect-style "◆" fg=#bb9af7`, `expect-style "billed" fg=#464c66`, `expect-style "<1%" fg=#7694d9`, `expect-style "▱" fg=#5a617e`, `snap --style after-turn`, `image {evidence}/status.png`. Expect: model, in/out/cache counters and session id on the last row.
- **Reset.** Drive: `type /new`, `key enter`, `wait "billed ↑0 ↓0"`. Expect: the total is back to zero.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/status-line`. Expect: `ok: /status shows session …` — the `/status` block's request count, input total and session id equal the `usage` and `head` rows in `journal.txt`.

## Gotchas

- At narrow widths a toast or tool-group labels can push the counters off the right edge (issue #67); drive at 100 columns or wider unless testing that.
- `billed ↑N` is cumulative billed prompt tokens, not context size; `ctx` is the context.
- Summary and sub-agent accounting is pinned by e2e tests (`compaction.rs`, `delegate.rs`), not this recipe.
- The cache rate, cost, warmer `ctx` colors and scroll marker are not driven here: the mock reports no cache hits or cost by default (`construct-dev serve --cache` simulates a prefix cache). Add them as their own feature when a recipe covers them.
