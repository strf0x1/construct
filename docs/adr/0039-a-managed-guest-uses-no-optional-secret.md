# 0039 — A managed guest uses no optional secret

Status: accepted. 2026-11-03. Completes the construct side of Deck ADR 0004
§10, after ADR 0037.

## Context

ADR 0037 made a Deck-managed guest resolve secrets from the environment only:
the marker `DECK_MANAGED` means the host owns every credential, and Deck
injects placeholders at the gateway. That removed the keyring, but `get()`
still returned whatever the guest environment held. A guest process can set
its own environment, so `EXA_API_KEY`, `BRAVE_API_KEY`, `TAVILY_API_KEY`, a
`TYPESAFE_API_KEY`, or an MCP server's declared secret could enable web
search or the Jev router inside the guest even though Deck supplied none of
them. Deck ADR 0004 §10 is explicit: a key in a guest environment is not
acceptable, even an optional one.

## Decision

**`secrets::optional` resolves nothing in a managed guest.** It is the
accessor for a secret an optional feature asks for: a web search backend, the
Jev router and learning gate/ledger, an MCP server's declared secrets, and
the `mcp` policy comment that says whether one is set. In a managed guest it
returns `None` without reading the environment. `get` — the endpoint key — is
unchanged: Deck supplies that placeholder deliberately.

**A named search backend tells the truth in a managed guest.** With no key,
`[tools.web] search = "exa"` reports that Exa's key is not set and that this
Deck task's credentials are host-owned, instead of telling the user to run
`construct secrets set`, which ADR 0037 already refuses. `auto` falls through
to the keyless DuckDuckGo backend, which still has no route out of a v1 guest.

## Consequences

- A guest cannot turn an environment key into web search or a Jev route by
  exporting it. Deck must inject a credential at a gateway for the feature to
  work, and v1 injects only the model key.
- An MCP server that needs a declared secret starts with that name absent, as
  it would on a host where the secret is unset.
- Host behavior is unchanged: `optional` and `get` are the same lookup
  without `DECK_MANAGED`.
- The marker remains a mode switch, not a boundary: the guest can unset it to
  change its own CLI behavior, but the host gateway still discards guest
  credentials (ADR 0037).

## Evidence

- Deck ADR 0004 §10 and construct ADR 0037.
- `crates/construct-dev/tests/deck.rs`: a managed guest with
  `EXA_API_KEY=guest-supplied` gets "credentials are host-owned" and the
  value never reaches the model; a managed guest with
  `TYPESAFE_API_KEY=guest-supplied` routes locally and writes no `jev.log`.
- `crates/construct/src/secrets.rs`: `optional` is the only accessor the
  optional consumers call; `get` keeps the endpoint path.
