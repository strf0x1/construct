---
id: status-line
status: verified
surfaces: [tui]
verified_at: ffa04de
---
# Status line

The bottom row of the TUI shows, left to right: a lavender `◆` and the current model, then on the right any toast or learning notice, the enabled tool groups, the running token total (`N tok`, every request's prompt plus completion tokens this session), the cache hit rate once anything was cached, the cost when the provider reports one, context use (`ctx N%`, yellow above 70%), and a scroll marker when scrolled up.

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
- **Look.** Drive: `expect " tok"`, `expect-style "◆" fg=#bb9af7`, `snap --style after-turn`, `image {evidence}/status.png`. Expect: model and token count on the last row.
- **Reset.** Drive: `type /new`, `key enter`, `wait "0 tok"`. Expect: the total is back to zero.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/status-line`. Expect: `ok: status shows N tok = sum of usage rows` — the snap's total equals the sum of prompt plus completion over `journal.txt`'s `usage` rows.

## Gotchas

- At narrow widths a toast or tool-group labels can push the counters off the right edge (issue #67); drive at 100 columns or wider unless testing that.
- The total is cumulative billed tokens, not context size; `ctx %` is the context.
- The cache rate, cost, `ctx %` color and scroll marker are not driven here: the mock reports no cache hits or cost by default (`construct-dev serve --cache` simulates a prefix cache). Add them as their own feature when a recipe covers them.
