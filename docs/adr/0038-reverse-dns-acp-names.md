# 0038 — ACP extensions use a reverse-DNS namespace

Status: accepted. 2026-11-03. Implements construct#171, the construct half of
deck#36.

## Context

ACP reserves `_`-prefixed methods for custom extensions, and in v2 also
reserves unprefixed `_meta` keys. The documented extension namespace is
reverse-DNS: methods named `_com.example/<name>` and `_meta` keys named
`com.example/<name>` (ACP v1 and v2 Extensibility).

Construct and Deck used bare names for their shared protocol: methods
`_construct/turn`, `_construct/event`, `_deck/boundary`, and `_meta` keys
`construct` and `deck`. A released Deck and a released Construct already
speak these names, and the two halves ship separately, so the rename is a
compatibility problem as well as a conformance cleanup.

## Decision

**The namespace is `com.sprawlcommons`.** The controlled domain behind
`construct.sprawlcommons.com` names it.

| Use | Name |
|---|---|
| turn state notification | `_com.sprawlcommons/construct/turn` |
| event notification | `_com.sprawlcommons/construct/event` |
| construct `_meta` key | `com.sprawlcommons/construct` |
| boundary notification | `_com.sprawlcommons/deck/boundary` |
| deck `_meta` key | `com.sprawlcommons/deck` |

**A client negotiates the notification names it gets.** In `initialize`:
`clientCapabilities._meta["com.sprawlcommons/construct"].events` asks for
whole events under the new method, `.turn` asks for turn state under the new
method. A client that names neither gets the bare method names it was built
for. A client that asks for events with the bare `_meta.construct.events` key
still gets `_construct/event`. The server advertises both construct keys
(`events`, `liveAttach`) and both deck keys (`boundary`), so either Deck
finds what it reads.

**Received names are accepted in both forms for one release.** `session/new`
reads `_meta` `start` from either construct key, the boundary notification
accepts `_deck/boundary` and `_com.sprawlcommons/deck/boundary`, and
`session/load` returns the turn snapshot under both construct keys.

The bare constants in `acp/map.rs` are marked `LEGACY_*`; remove the
advertising, the acceptance and the constants one release after this lands.

## Consequences

- New + new uses only the new names. New Construct + old Deck: the old
  capability keys, old boundary method and old turn method keep attach and a
  late decision working. Old Construct + new Deck: the new Deck prefers the
  new capability, falls back to the old, and sends the method the agent
  advertised.
- A third party implementing an ACP client reads one naming rule, not a
  project-specific exception.
- The transition costs a small amount of code and two extra `_meta` keys in
  snapshots and capabilities. It does not change payloads.
- Revisit when the bare names are removed; a new extension name added before
  then uses the new namespace from the start.

## Evidence

- construct#171 and deck#36 (acceptance matrix).
- `crates/construct-dev/tests/acp.rs`: new-key client gets
  `_com.sprawlcommons/construct/event`; legacy-key client gets
  `_construct/event`; both capability keys and both load snapshot keys.
- `crates/construct-dev/tests/acp_live.rs`: a client naming `.turn` gets the
  new turn method and no bare one; a client naming nothing keeps the bare
  method.
- `crates/construct-dev/tests/deck.rs`: both boundary capability keys are
  advertised; both boundary methods move a late decision.
