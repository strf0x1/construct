---
id: peers
status: verified
surfaces: [tui]
depends: [crates/construct-tui/src/peers, crates/construct/src/cli/hub/tui.rs, crates/construct-hub/src/status.rs, crates/construct-hub/src/state.rs, crates/construct-hub/src/conn.rs, crates/construct-hub/src/remote.rs, crates/construct-hub/src/view.rs, crates/construct-hub/src/model.rs, crates/construct-dev/tests/peer_tui.rs, crates/construct-dev/tests/peer_llm.rs]
verified_at: 98909a9
verified_deps: 4c345556a404ae3f
---
# Peers in the TUI

With `construct hub` running for the same home, the TUI shows a tile in the upper right: one row per peer with its state (`pending` with its SAS code, `online`, `working`, `away`, `stale`, `revoked`, or `expected` for an invite nobody used yet) and `?N` when the peer is waiting on N permission questions, then the model its calls use (its last path segment), in lavender with `(hub)` when the hub set it. A join waiting for approval opens a JOIN popup with the peer's name, its address, the code the peer prints, and a countdown; approving pins its key and nothing is typed on the peer. A peer's permission question opens a PEER popup that names the peer and says the question is about its own machine; "Always" says it is saved to that peer's config. `/peer <name> <prompt>` sends a prompt, `/peer <name>` opens a read-only view of its session over the transcript, `/peer <name> --model <selector>` switches the model its hub-proxied calls use (`--clear` goes back to its own; ADR 0078), `/peer` lists the peers. No hub, no tile; the first frame never waits on one.

## Sub-features

- `peers-tile` one row per peer with its state, code and open-question count.
- `peers-join` a pending join opens the JOIN popup; `a` approves (pins the key), `d` denies, esc puts it aside; it expires after five minutes.
- `peers-ask` a peer's permission question opens the PEER popup; `y` `s` `a` `n` answer with the peer's own options, and the hub refuses an answer whose question moved.
- `peers-prompt` `/peer <name> <prompt>` sends a prompt to a pinned peer.
- `peers-model` `/peer <name> --model <selector>` sets the hub's model override for that peer, kept in `hub/peers.json`; the tile marks it `(hub)`; `--model --clear` removes it, `--model` alone says which model it uses.
- `peers-attach` `/peer <name>` shows the peer's session; esc closes, pgup/pgdn scroll, the input stays usable.
- `peers-grace` keys pressed in the first 450 ms after a popup appears are ignored.

## How to get to it (user POV)

- Run `construct hub` (and `construct hub pair` or `construct hub invite <id>`), start a peer, then open `construct` in the same home: the tile appears within a second.
- Inside a popup: `up`/`down` and `enter`, or the letter keys; `esc` puts it aside (the tile keeps showing it; `/peer <name>` brings it back).

## Done when

- The tile shows `old-pc pending <code>` and the JOIN popup shows the same code the peer printed; approving leaves `old-pc` pinned in `hub/peers.json` and the tile reads `online`.
- After `/peer old-pc <prompt>`, a read outside the peer's sandbox opens the PEER popup naming `old-pc`, with `Always allow · saved to old-pc's config`; answering `y` lets the read run on the peer, with an `access` row `"decision":"once"` in the peer's journal.
- `/peer old-pc` shows the peer's transcript, ending with its model's reply.
- After `/peer old-pc --model small`, the tile reads `old-pc online small (hub)` in lavender and `hub/peers.json` has `"model": "small"` on old-pc's row.

## Driving it

Preconditions:

- `setup.sh` starts a hub in the TUI's home with its pairing window open, and a peer `old-pc` with its own home, work dir and scripted model (read `{outside}/secret.txt`, then `peer finished`), waiting for approval; `teardown.sh` stops them. Recipe: `.agents/skills/verify-construct/recipes/peer-tile`.

- **Join.** Drive: `wait "old-pc pending" 8000`, `wait "wants to join this hub"`, `sleep 600`, `snap --style join`, `expect-style "Approve · pins its key" fg=#9ece6a bg=#2c3048 bold`, `image {evidence}/join.png`, `type a`. Expect: `approved: its key is pinned`, then `old-pc online`.
- **Ask.** Drive: `type /peer old-pc read the secret`, `key enter`, `wait "about its own machine"`, `sleep 600`, `expect-style "Always allow · saved to old-pc's config" fg=#e0af68`, `image {evidence}/ask.png`, `type y`. Expect: `answered`, and the popup is gone.
- **Attach.** Drive: `type /peer old-pc`, `key enter`, `wait "peer finished"`, `image {evidence}/attach.png`, `key esc`. Expect: the view closes.
- **Model.** Drive: `type /peer old-pc --model small`, `key enter`, `wait "old-pc's model calls now go to small"`, `wait "small (hub)"`, `expect-style "small (hub)" fg=#bb9af7`, `image {evidence}/model.png`. Expect: the override in the tile.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/peer-tile` (runs it after the drive). Expect: `ok: old-pc pinned; popup showed code <code>; peer read ran after an answer of once; model override small kept` — it reads `hub/peers.json`, the code from the peer's log, and the peer's journal.

## Gotchas

- Letters are sent with `type`, not `key`: `key a` is not a key name.
- The popup's border breathes between cyan and lavender, so `settle` never returns while it is open, and the badge's background changes from frame to frame; check its foreground only.
- The tile is polled once a second, so a `wait` for a state change needs a budget above that.
- The join's countdown is the hub's (`APPROVAL_TTL_SECS`, five minutes); the recipe approves long before it.
