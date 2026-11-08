# 0043 — optchat: the whole history as a tree of short lines

Status: proposed, 2026-11-07. Implements the second strategy behind ADR 0042.
Builds on 0008 (durable journal), 0012 (prompt caching) and 0013 (compaction).
Answers #197.

## Context

ADR 0042 made context management a strategy chosen per session, so construct
can also be the long-running coordinator Deck plans. That coordinator holds
one conversation per body of work for weeks. It starts tasks, reads their
reports and keeps decisions. Its turns are few, its tool output is small, and
what it needs most is to remember what was decided long ago. The `compact`
strategy loses exactly that: each cut replaces older rounds with a summary,
and the next cut summarizes that summary again.

Victor Taelin's optchat
(<https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449>),
implemented in UniiChat (<https://github.com/VictorTaelin/UniiChat>), keeps
every message. Each message is summarized once, in 512 bytes or less, and
summaries are merged pairwise into a tree. The model sees one line per node,
covering the whole history. Old history appears in coarse lines and recent
history in fine ones, and nothing is missing. The model can reopen a line
with `zoom`.

The options for what the model sees of the most recent turns:

- **A: as UniiChat does it.** System prompt, then the view, then only the
  current turn raw. Earlier turns appear only as lines. Short messages are
  their own lines word for word, so a coordinator's typical exchange survives
  intact. Between turns the view only grows at its end, which keeps the
  cached prefix stable. This is the shape that was measured.
- **B: a hybrid** that also keeps the last K turns raw. It reads more
  naturally, but the raw window's start moves every turn, so the prefix
  changes and the cache breaks every turn. It was not measured.

This ADR takes A. B stays a possible bench arm.

## Decision

1. **`[context] strategy = "optchat"` selects it, and it is opt-in.** Its
   settings live in `[context.optchat]`:
   - `model`: the summary model. Empty falls back to `[compaction] model`,
     then to the run's model.
   - `high`: the view's size limit, default 128000 bytes.
   - `concurrency`: summary calls at once, default 8.

   The strategy refuses a model whose context window is under 65536 tokens.
   The view alone can reach about 32k tokens, so optchat is for big-window
   models only; small windows stay with `compact` (#95). The check runs when
   the agent is built, where the window is known, and the error names both
   numbers.

2. **The log and its leaves.** Every message the agent pushes, after the
   system prompt, is a log entry. Its index is its position among the
   session's message events, so the journal already holds the log and
   nothing is stored twice. Each entry has one leaf, its line text:
   - An entry whose flattened text fits in 512 bytes is its own leaf, word
     for word, with no model call. A role label (`user:`, `assistant:`, a
     tool call's `name(args)`) is part of that text.
   - A longer entry is summarized by the summary model into 512 bytes or
     less. The summary's input is clipped to 30000 characters, keeping the
     head and the tail, as the gist does.
   - A `zoom` call or result gets a fixed short leaf naming what was opened,
     with no model call, so summaries are never summarized again.

   The 512-byte limit is part of the format and never changes, because
   replay depends on it.

3. **The tree.** Node `(l, i)` covers entries `[i·2^l, (i+1)·2^l)` and is
   written `{i·2^l}+{2^l}|text`, newlines flattened (UniiChat's `ref` and
   `line`). A parent is the summary model's merge of its two children,
   again in 512 bytes or less. Each node is written once and never
   rewritten.

4. **The view.** The view is an ordered list of nodes that covers the log
   from entry 0 to the start of the current turn, each entry exactly once,
   oldest first. Each entry the view reaches adds its leaf at the end. When
   the view passes `high` bytes, a batch replaces the most "due" adjacent
   sibling pairs with their parents until it is at or below half of `high`.
   A pair's age is counted from its last entry, so older history coarsens
   first and the view changes only near the lines a batch touches. A batch
   uses only parents already written. If none is written, it writes the most
   due ones first.

5. **The request** is:

   ```
   system   unchanged for the session's life
   user     <chat>, the view in blocks of 4 lines, </chat>, then the turn's opening message
   …        the current turn's assistant messages and tool results, raw
   ```

   On the next turn, the last turn's raw messages are gone and come back as
   leaves at the end of the view. The model never sees an earlier turn raw
   unless it zooms.

6. **Caching needs marked parts, which amends ADR 0042 decision 7.** Under
   Anthropic's prefix caching a changed block misses entirely, so a view
   sent as one text block would miss every turn. UniiChat splits the view
   into 4-line blocks and marks the last complete one (`Memory.view`,
   `BLOCK = 4`). Blocks before it are unchanged between turns, so they hit.
   So:
   - `llm::Message` gains `parts: Vec<Part { text, mark }>`, additive, empty
     by default, and serialized only when non-empty. A message with parts
     sends them as content parts.
   - The dialect puts `cache_control` on a marked part. `Request` keeps its
     top-level mark as well. `LastPrompt`, which honours one breakpoint,
     uses the marked part as that one breakpoint instead of the newest
     prompt.
   - `compact` never sets parts, so its requests and the golden snapshots
     don't change.

   This is the one thing a strategy now reports to the dialect: which part
   to cache.

7. **When nodes are written.** All summary calls happen inside `fit`,
   because a strategy has no client of its own (0042).
   - At a turn's first request, `fit(Threshold)` writes every missing leaf
     for the previous turn's entries, up to `concurrency` at once. The turn
     waits for them, because the view must have no holes (decision 10). The
     same `fit` writes any parents whose children both exist, up to
     `concurrency`, and runs a batch if the view is over `high`.
   - Within a turn, results stay raw. If the request passes the 0013
     threshold, or the provider rejects it as too long (`Reason::Overflow`),
     `fit` folds the turn's older rounds into the view at a round boundary,
     so a tool result never loses its call. The turn's opening message stays
     raw at the end of the user message. `NothingToCut` means only the
     opening message and the newest round are left.

   Writing nodes in the background, while the main request runs, is a later
   optimization. It needs the agent to drive the strategy between requests.

8. **Journal events.** A `fit` that changed anything returns one
   `Fit::Changed(Event::Memory { … })`. That is an additive variant with:
   - `leaves: [{ i, text, fallback }]`: model-written leaves only, since
     short leaves are derived from the entry;
   - `merges: [{ l, i, text, fallback }]`;
   - `view: Option<[(l, i)]>`, set when a batch or a fold changed the view's
     shape;
   - the summary calls' usage and `ms`.

   `Context::replay` rebuilds the tree and the view from these events and
   the message events, with no model call. Under `Check::Strict` it refuses
   a node past the log's end, a parent whose children aren't in the tree,
   and a view that doesn't cover its range exactly once. That keeps 0042
   decision 4: the view is replayed, never recomputed.

9. **`zoom` is the only tool, offered through `Context::tools()`.** Its wire
   name is declared once.
   - `zoom(id, n)` on a node returns its two children's lines.
   - `zoom(id, 1)` returns the entry raw, in pages of 32 KB, with a `page`
     argument.
   - An unknown or misaligned `(id, n)` is an error that says what's valid.

   There is no search tool. Taelin leaves it out on purpose, and an agent on
   optchat keeps its hard facts in structured state (0042 decision 6). omp's
   `chat_search` can be a later bench arm.

10. **The view stands on its own (0042 decision 5).** Every log entry before
    the current turn is covered by exactly one line, and the turn waits
    until every line is written. A summary that fails or is refused still
    yields a line: the entry's head cut to 512 bytes and marked
    `[unsummarized]`, journaled with `fallback: true`, and recorded as a
    note. The model can therefore always continue without `zoom`. `zoom`
    adds detail, not coverage.

11. **Summary calls.** The summary model has its own fixed system prompt:
    one line of at most 512 bytes that keeps names, ids, numbers and
    decisions. A line that comes back over 512 bytes is retried up to 5
    times, keeping the shortest, then cut at a UTF-8 boundary. The
    strategy measures bytes; it never trusts the model to count them.

12. **Learning reads the log, not the view.** `Context` gains
    `fn log(&self) -> &[Message]`, which defaults to `messages()`. For
    optchat it returns every entry. `tui/learning.rs` counts and slices
    `log()`, because the view's length goes down when a turn folds.

## Consequences

- A coordinator keeps every decision at some resolution for the life of the
  session, at the cost of one cheap summary call per long message and about
  one per merge. Short messages cost nothing. While history still fits the
  window, optchat costs more than `compact`. omp's README reports $0.41–0.43
  a run against $0.32 without it.
- A turn starts after the previous turn's leaves are written, so it waits
  about one summary call's latency. A coordinator can afford that; a
  tool-heavy coding task can't, which is why `compact` stays the default.
- `llm::Message` gets `parts`, and the dialect learns per-part marks. Every
  provider dialect has to keep compiling and snapshotting the same for
  messages without parts.
- Under A, a long last reply comes back as its summary on the next turn. If
  the bench shows the model losing the thread of its own last answer, B is
  the arm to try.
- Hard facts (task ids, hashes, PR numbers) will get garbled after enough
  merges. Deck's coordinator reads them from `deckd` (0042 decision 6);
  this ADR does not try to protect them.
- A session keeps optchat for its life (0042 decision 4). There is no
  migration from `compact` into a tree.
- Revisit if the bench shows no recall gain past the window, if cached share
  under `LastPrompt` stays low, or if background node writing becomes
  necessary for latency.

## Evidence

No measurements yet. The first slices, to file as issues when this is
accepted:

1. `Message::parts` and per-part cache marks in the dialect. Neutral for
   `compact`: golden snapshots unchanged.
2. The `optchat` strategy: log, leaves, tree, view, `Event::Memory`, replay
   and the window check. Tested with a fake `Complete`; replay makes no model
   call.
3. `zoom`, `Context::log` and learning.
4. A bench family: a long synthetic coordinator transcript with decisions
   planted far back, then questions that need them. It compares `compact`
   against `optchat` on recall, cost and cached share (#187's metrics), and
   relates to #128.

Read in UniiChat at its current `HEAD`:
- `Memory.ts`: `line`, `view` with `BLOCK = 4` and one marked block, `HIGH =
  128000`, `TRIES = 5`, batch "due" counted from a pair's last message, and
  only built parents merged.
- `Anthropic.ts`: per-part `cache_control`, plus the system prompt and a
  top-level mark.
- `Agent.ts`: the view's blocks open the first user item, and the turn
  follows raw.

Where the code and the gist disagree:
- `JOBS = 64` in code against 8 in the gist. This ADR takes 8.
- The code keeps an over-long line after its tries. This ADR cuts it.
- `Memory.whole` would send recent messages whole up to `HIGH`, and no
  caller was found in `Memory.ts`, `Agent.ts` or `Anthropic.ts`. This ADR
  does not send them.

The omp-optchat README (<https://github.com/ericjuta/omp-optchat>) reports:
- past the window, 8/8 on each of three seeds with optchat, against 1/8, 7/8
  and 8/8 without it;
- within the window, 24/24 at higher cost;
- n = 3, and the author makes no savings claims;
- Claude 5.x refuses about 1 in 100 summary calls, which decision 10's
  fallback covers.

pi-optchat (<https://github.com/jonaslsaa/pi-optchat>) folds mid-turn results
at 30% of the window. Decision 7 uses construct's existing threshold
instead.
