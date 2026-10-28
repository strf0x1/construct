---
id: status-line
status: verified
surfaces: [tui]
verified_at: ffa04de
---
# Status line

The bottom row of the TUI shows, left to right: a lavender `◆` and the current model, then on the right any toast or learning notice, the enabled tool groups, the cumulative counters (`N in · N out · cache N%`, summed over every request this session), the cost when the provider reports one, context use (`ctx N%`, yellow above 70%), a scroll marker when scrolled up, and the short session id. The counters and id are never trimmed: at narrow widths the toast is cut first, then the groups, then the model name. `/status` draws a rounded `status` box (session id, journal path, model, context, requests, input/cached/output/cost, summed from the journal) and `/copy status` copies it.

## Sub-features

- `status-model` the current model next to a lavender `◆`.
- `status-tokens` the session's token total, updated after each request.
- `status-reset` `/new` sets the total back to zero.

## How to get to it (user POV)

- Always visible at the bottom of the TUI; it updates as a turn runs.

## Done when

- After a turn, the bottom row shows the model and a non-zero `tok` count; `/new` resets the count.

## Driving it

Preconditions:

- Baseline state. Model: one text reply `hello from the mock`. Recipe: `.agents/skills/verify-construct/recipes/status-line`.

- **Turn.** Drive: `type say hi`, `key enter`, `wait "hello from the mock"`, `settle 300 5000`. Expect: the reply.
- **Look.** Drive: `expect " in · "`, `expect-style "◆" fg=#bb9af7`, `snap --style after-turn`, `image {evidence}/status.png`. Expect: model, in/out/cache counters and session id on the last row.
- **Reset.** Drive: `type /new`, `key enter`, `wait "0 in"`. Expect: the total is back to zero.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/status-line`. Expect: `ok: /status shows session …` — the `/status` block's request count, input total and session id equal the `usage` and `head` rows in `journal.txt`.

## Gotchas

- At narrow widths a toast or tool-group labels can push the counters off the right edge (issue #67); drive at 100 columns or wider unless testing that.
- `N in` is cumulative billed prompt tokens, not context size; `ctx %` is the context.
- The cache rate, cost, `ctx %` color and scroll marker are not driven here: the mock reports no cache hits or cost by default (`construct-dev serve --cache` simulates a prefix cache). Add them as their own feature when a recipe covers them.
