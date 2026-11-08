# 0042 — Context management is a strategy, chosen per session

Status: proposed, 2026-11-07. Builds on 0008 (durable journal), 0012 (prompt
caching) and 0013 (compaction planner). Answers the compaction question in #100.

## Context

How history fits the window is one fixed policy today. The ADR 0013 planner
stubs old tool results, then summarizes older rounds. It is spread across
three places that all assume it:

- `agent/compaction.rs` plans a `Cut`, calls the summarizer, applies the cut
  to `Agent::messages` and journals `Event::Compacted`;
- `journal/read.rs` `project` replays `compact::apply` for resume;
- `journal/recovery.rs` `repairs` replays it again to check a journal can be
  restored safely.

That policy suits what construct mostly runs: a tool-heavy task that reads
and edits files and ends in a diff. It suits a different agent badly. Deck
(the desktop client that runs construct in per-task VMs) plans a
**coordinator**, one long conversation per body of work. It starts tasks,
reads their reports and keeps decisions over weeks. Its turns are few and
valuable, its tool output is small, and what it needs most is to remember
what was decided a long time ago. A summary that replaces older rounds loses
exactly that, a little more at each cut.

Victor Taelin's optchat
(<https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449>)
handles this case differently:

- every message is kept, and each gets a summary of at most 512 bytes;
- summaries are merged pairwise into a tree, still 512 bytes a node;
- the model sees one line per node, covering the whole history in roughly
  64–128 KB, and can reopen a line with `zoom`.

It adds a cheap summary call per message, which a coordinator can afford and
a tool-heavy task cannot. The user's hypothesis, which this ADR adopts, is
that the two agents need different strategies, and that construct should
host both rather than Deck writing a second agent loop.

The options considered:

- tune the 0013 planner for long sessions (#95 is already doing this for
  small windows): it stays one policy, and a summary-of-summaries still loses
  old decisions;
- a separate coordinator harness: duplicates the loop, tools, journal and ACP
  server;
- **make the policy a strategy behind one interface**, the way `tools.editor`
  swaps the read/edit family and `learn.strategy` swaps learning.

## Decision

1. **A `Context` strategy owns the message list.** It lives in a new
   top-level module, `crates/construct/src/context/`. The agent sends what
   the strategy's view returns, and appends every message through it. A
   strategy is responsible for five things:
   - observing each message as it is appended;
   - producing the view to send under a given budget;
   - reacting to the threshold before a request, and to a provider's "too
     long" rejection;
   - contributing tool definitions of its own (none for `compact`);
   - replaying its journal events to rebuild the same view on resume and
     recovery.

   What stays in the agent: detecting an overflow from the provider's error
   (`compact::is_overflow`, `overflow_limit`), the window and budget, sizing
   a request, pricing usage, and emitting events.

2. **`[context] strategy` selects it**, default `"compact"`.
   `CONSTRUCT_CONTEXT_STRATEGY` sets it for one run, like
   `CONSTRUCT_TOOLS_EDITOR`. An unknown name fails config loading with the
   list of known names. The strategy's name is recorded in the `Manifest`
   (so `head` carries it), and `construct manifest` prints it.

3. **The ADR 0013 planner becomes the `compact` strategy, unchanged.** Same
   thresholds, the same `[compaction]` table, the same `Event::Compacted`
   payload, the same summarizer and its `[compaction] model`. Old journals
   resume exactly as before. Golden request snapshots do not change.

4. **A strategy's state is journal events, never recomputed.** Anything a
   strategy decides that changes the view (a cut, a summary node) is an
   `Event`, either a variant or a defaulted field, additive per 0008. The
   projector replays it. Recomputing on resume would change the view the
   model saw and break the prompt cache (optchat's author lists rebuilding
   on restart as a failure mode). A session resumes with the strategy its
   `head` manifest names. If config names another one, resume refuses and
   says why, rather than replaying one strategy's events with another.

5. **The view must stand on its own.** This is how "the journal is not a
   compaction strategy" (#29 decision 7, 0008) reads from now on. A strategy
   may give the model retrieval tools (optchat's `zoom`), but the model must
   be able to continue correctly from the view alone, without calling them.
   A strategy whose view covers the whole history at some resolution meets
   this rule. One that drops history and relies on the model choosing to
   look it up does not.

6. **Hard facts live outside chat memory.** A strategy that compresses
   heavily will eventually garble a task id, a PR number or a hash. An agent
   that runs on such a strategy keeps its authoritative state somewhere
   structured that it reads through tools. For a Deck coordinator that is
   `deckd`'s task and space records. Chat memory holds the narrative and the
   decisions. This is a rule for agent design, not for this interface.

7. **Prompt caching carries over.** Per 0012, a strategy reports nothing new
   to the dialect. A strategy should keep the view's prefix stable between
   requests, and the bench judges it with the cache metrics #187 adds.

## Consequences

- An optchat strategy can land beside `compact` without touching the loop,
  and a bench arm can compare them on the same tasks. Its design (node
  events, `zoom`, the summary model, cost) is its own ADR.
- Deck's coordinator can run construct with `[context] strategy = "optchat"`
  and its own tool set. Deck needs no agent loop of its own.
- #95's small-window work changes only the `compact` strategy and is not
  blocked by this.
- `Agent::messages` stops being a public field. Readers (`route`, TUI
  learning, `wake`, `deck`) go through the strategy's view. That is a
  mechanical change, but it touches several files.
- Learning reads the view today (`tui/learning.rs` indexes
  `agent.messages`). This works for `compact`, where the view is what
  happened plus cuts. A strategy whose view is not a superset of recent
  messages must give learning the raw messages. The optchat ADR settles
  this.
- Hot-swapping a strategy mid-session (#100's last question) is ruled out
  by decision 4. A session keeps one strategy for its life.
- Revisit if a second strategy never lands, or if the trait has to grow a
  method that only one strategy uses.

## Evidence

Implementation: #195 (the seam, with `compact` behind it), #196 (replay
and resume through the strategy), #197 (design of the optchat strategy).

No measurements yet: the first step changes no behaviour. It is judged by
the golden request snapshots staying unchanged, the full `cargo xtask check`,
and a `compact-*` bench comparison of `main` against the branch with every
metric `same`. The optchat strategy's evidence belongs to its own ADR.

Sources: Taelin's optchat gist (linked above). UniiChat
(<https://github.com/VictorTaelin/UniiChat>) and the oh-my-pi and pi ports
(<https://github.com/ericjuta/omp-optchat>,
<https://github.com/jonaslsaa/pi-optchat>) are implementations to read
before the optchat ADR. They are not cited for any claim here.
