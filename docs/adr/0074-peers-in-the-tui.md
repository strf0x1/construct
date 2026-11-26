# 0074 — Peers in the TUI: a polled tile, popup-only answers, a five-minute join

Status: accepted. 2026-11-25. Part of #273, #365. Builds on ADR 0016, 0069,
0070, 0071.

## Context

The hub (ADR 0069) holds joins waiting for a human (ADR 0070) and each
peer's permission questions (ADR 0071), but until now a person reached them
only with `construct hub pending|approve|answer`. #365 brings them into the
TUI. Constraints:

1. The TUI may not name `construct-hub` (`used_only_by` in
   `docs/architecture.toml`), and the hub must keep running with no TUI.
2. The first frame waits on nothing, including a hub that is not running.
3. A peer's permission question must never be answerable by the
   coordinator's model, and must not be mistaken for a local question, where
   "Always" writes this host's `config.toml`.
4. A join held for a human had no end: it lasted as long as its link, so a
   join nobody looked at sat in the pending slots (`MAX_PENDING`) for hours.
5. The hub's `answer` named a question by its place only. A TUI polls, so
   between the poll it drew and the key a person presses, the list can move:
   an earlier question is answered elsewhere, a new one is asked.

## Decision

- **The seam is a trait the TUI owns.** `construct_tui::PeerHub` has three
  calls: `peers()` (rows; `None` when no hub runs), `act(Act)` (approve,
  deny, prompt, answer) and `session(id)` (the attach view's text). The
  binary implements it over the control socket (`cli/hub/tui.rs`) behind its
  `hub` feature and passes it to `construct_tui::run`; without `hub` it
  passes `None` and `/peer` says the build has no hub.
- **The TUI polls once a second, after the first frame,** from a background
  task that talks to the UI only through a channel (`peers/poll.rs`). Each
  call opens its own connection, so a hub started or stopped while the TUI
  runs is found or missed on the next poll. No hub, no tile.
- **The hub reports facts; the TUI names the state.** `PeerStatus` gains
  `working` (a prompt in flight or a turn not idle), `questions` (title,
  option ids and the peer's wording) and `expires`. The tile's words
  (`pending`, `online`, `working`, `away` under fifteen minutes offline,
  `stale`, `revoked`, `expected`) come from one pure function.
- **A peer is answered only by a key in the peer popup.** Nothing on the
  agent's side holds a `PeerHub`, so no tool and no model output can reach
  `act`. The popup is not the local panel: its badge names the peer
  (`⇄ PEER · old-pc`, `⇄ JOIN · old-pc`), its border breathes cyan and
  lavender, its intro says the question is about the peer's own machine,
  and "Always" reads `saved to <peer>'s config`. It keeps the local panel's
  450 ms key guard (now `theme::GRACE`, shared by both). Esc puts an item
  aside, never answers it; the tile keeps showing it and `/peer <name>`
  brings it back. A local question takes the keys first.
- **A join waits five minutes** (`APPROVAL_TTL_SECS`). The link's task turns
  it away with `JoinAck { accepted: false }` when nobody decided; the popup
  counts it down.
- **An answer may name the question's title,** and the hub refuses it when
  the question at that place has another title by now. The TUI always sends
  it; `construct hub answer` still names the place only.
- **The attach view is read-only** and lies over the transcript's rows, so
  `/peer <name> <prompt>` can be typed while watching. Peer text is escaped
  before it is drawn (it is remote input).

## Consequences

- The tile is up to a second stale; that is the poll interval, not a lag a
  person acts on, since every decision is re-checked by the hub (a gone join
  is "no join is waiting", a moved question is refused).
- Five minutes is short for walking across a building; the peer redials and
  shows a new code, so a missed window costs one more look at the code, not
  a re-stamp.
- A future `peer` tool (#366) must not be given `act(Answer | Approve)`; the
  trait can be split then if it needs `Prompt` alone.
- Remote content in the popup is escaped, but the title is still the peer's
  words; the badge, colors and intro are what keep it from passing as local.

## Evidence

- `verify-construct` recipe `peer-tile`: a real hub and peer; the join popup
  shows the code the peer printed, approving pins it in `hub/peers.json`, a
  peer's read outside its sandbox opens the PEER popup, answering `y` runs
  the read on the peer with an `access` row `"decision":"once"`.
- e2e `peer_tui::a_join_is_approved_by_its_code_and_a_peer_question_is_answered_from_its_own_popup`:
  the same through `deny`, plus the 450 ms guard (an early `a` leaves the
  popup up).
- Unit tests: tile states (`peers/tile.rs`), popup wording and wrapping
  (`peers/panel.rs`), item order and set-aside (`peers/mod.rs`), the title
  guard (`questions.rs` via `remote.rs`).
