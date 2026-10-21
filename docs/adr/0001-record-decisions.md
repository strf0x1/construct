# 0001 — Record decisions as ADRs

Status: accepted, 2026-10-21.

## Context

Most of construct is written by agents, across many sessions. A decision that
lives only in a PR thread, a code comment or `observations.md` gets re-argued,
or silently undone, by the next agent that does not find it. Proposals in
`docs/proposals/` say what we plan; nothing says what we settled.

## Decision

A decision that constrains future work gets an ADR in `docs/adr/`, numbered in
order, from `template.md`: Context, Decision, Consequences, Evidence. A
proposal becomes an ADR when it lands. The ADR links the PR, and for anything
measured, the bench result files and the `bench.py compare` table, with every
number, including the ones that went against us.

ADRs are not edited after the fact except to set `Status: superseded by NNNN`.
A changed decision is a new ADR.

## Consequences

- An agent checks `docs/adr/` before changing a settled behaviour, and writes
  one when it settles one.
- Evidence is a result file with a manifest (proposal 05), so any number in an
  ADR can be traced to the build and config that produced it.

## Evidence

Proposal 03 phase 4, proposal 05 ("post the number either way").
