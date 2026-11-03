# 0036 — Deck's boundary reaches the agent through tools and one note

Status: accepted. 2026-11-03. Implements #166, #167 and #168. Implements the
construct side of Deck ADR 0004 §5, §6 and §10.

## Context

Deck runs construct in a VM whose only egress is a host gateway. When policy
holds or refuses a request, the guest sees a generic connection failure or a
`403`; the agent cannot tell policy from an outage and may look for another
route. The gateway already returns a stable body, and Deck will expose a
read-only boundary endpoint to the guest and push late decisions over ACP.

Three questions had to be settled here:

- how a process knows it is in a Deck task, without a second execution mode;
- where "this tool was blocked" lives, given that the blocked syscall happens
  in a tool process that cannot see the gateway;
- how a late decision reaches the model without becoming a hidden side channel.

## Decision

**The signal is `DECK_BOUNDARY_URL`.** `Config::load` reads it into
`Config.deck`; nothing else changes about ordinary runs. `Catalog::build_with`
adds `boundary_wait`, `boundary_status` and `boundary_withdraw` only when that
URL is present, so a normal session's catalog, manifest and first request are
unchanged. A delegated child clears the field: it shares the task, but the
parent's time-based note already covers blocks its delegates cause.

**Tool results carry the boundary note.** Before a tool runs, the agent reads
the gateway's current sequence from the shared `Boundary` handle; after it
exits, it fetches `/events?after=<seq>` and appends the note to the result the
model reads (and to the journaled `tool_end`). The cursor is initialized once,
discarding history from before this session, so old blocks are not attributed
to the first tool. An unreachable endpoint skips the note and never fails the
tool. Attribution is by time; the text says "while this ran", which is also the
truth for concurrent delegates.

**The three tools are the agent's choice.** `boundary_wait(id, timeout_s)`
long-polls `/approvals/<id>?wait=<s>` (own timeout, clamped to the host's
300 s) and is interrupted by `session/cancel` through `Ctx.cancel`;
`boundary_status` asks with `wait=0`; `boundary_withdraw` posts the withdrawal.
Their descriptions carry the guidance that keeps the model from routing around
the block. Nothing on the endpoint can allow an item.

**Late decisions are queued, journaled and delivered once.** `initialize`
advertises `agentCapabilities._meta.deck.boundary` only when `Config.deck` is
set. The reader loop accepts `_deck/boundary`, targets the named `sessionId`
when present and every session otherwise, and queues the notice on the
session's `Boundary` handle. The queue skips ids already resolved through a
tool (a terminal state marks the id seen), caps itself, and is drained before
the next model request — mid-turn or at the next turn. Delivery appends a
user-role `[deck boundary]` message and emits `Event::DeckBoundary { ids,
message }`, which is a model-message carrier: a resume or compaction replay
sees exactly what the model saw in that turn.

**Gateway denials are classified, not retried blindly.** `llm::deck::Denial`
reads Deck's `deck_` code from the body (and `X-Deck-Approval`), keeps the
approval id and scope in the error the agent sees, and marks only
`deck_upstream_failed` retryable. A 503 `deck_credential_missing` therefore
fails after one attempt while a provider 503 still retries. A model-gateway
denial ends the turn with that error; construct does not wait on the agent's
behalf — a later decision arrives through the same `_deck/boundary` note.

## Consequences

- Ordinary (non-Deck) runs are byte-identical in their model requests: the
  config field is skipped when absent, and the three tools are not in the
  catalog.
- Every tool in a Deck task makes one bounded loopback request after it runs,
  and the first makes a second to establish the cursor. A dead endpoint costs
  the note, never the tool.
- A mid-turn late decision breaks the provider prefix cache for the rest of
  that turn. Approval decisions are rare and the ADR asks for "within the
  current turn", so correctness wins.
- Concurrent read-only tools can attach a block to whichever finishes first.
  Shell calls, the acceptance case, are sequential.
- The guest is untrusted: it can point `DECK_BOUNDARY_URL` at its own server or
  unset it. That can only feed the agent false notes; the endpoint cannot
  allow anything and no host credential crosses it.
- Deck must emit `approval_id`/`X-Deck-Approval`, set `DECK_BOUNDARY_URL`, and
  serve `/events` and `/approvals/<id>`; until then the construct tests use a
  fake endpoint.

## Evidence

- Deck `docs/adr/0004-host-egress-and-credentials.md` §5, §6 and §10;
  `crates/deck-gateway/src/http.rs` (`write_error`) fixes the body shape.
- `crates/construct/src/llm/deck.rs` and its unit tests: nested and flat body
  shapes, the header fallback, and `deck_upstream_failed` as the only retryable
  code.
- e2e `deck::*` (`crates/construct-dev/tests/deck.rs`, a fake boundary server
  in `tests/common/boundary.rs`): credential denial fails after one request; a
  403 names the approval and scope; provider 500 and `deck_upstream_failed`
  retry; a blocked shell result carries the note in the request and journal;
  `boundary_wait` returns `allowed` with scope and honours `session/cancel`;
  status and withdrawal work; `initialize` advertises the capability; a late
  decision appears exactly once on the next request, mid-turn or after a turn,
  and not at all after a status call already resolved it.
