# docs

Where a thing is written down depends on how settled it is. README.md at the
root says why construct exists; AGENTS.md says how to work on it. This
directory holds the rest.

## proposals/

Plans that have not fully landed: what we intend to build, why, in what steps,
and how to tell each step is done. They are numbered in the order they were
written (`01-…`). A proposal is a working document and may be wrong; it is
allowed to say "plan, not started". Read one before starting the work it
describes, and update its status line as steps land.

## adr/

Architecture decision records: what we settled, and the evidence. One file per
decision, numbered in order (`0001-…`), written from `template.md` (Context,
Decision, Consequences, Evidence). `0001-record-decisions.md` is the rule
itself. An ADR is not edited after the fact except to mark it superseded; a
changed decision is a new ADR. Check here before changing a behaviour that
looks arbitrary: it may have been measured.

## rust-eval.md

How to score a model on Rust work in this repo: the `rust-replay-*` bench
tasks, the convention lint, and the rubric for the blind judge pass.

## How a proposal becomes an ADR

When a proposal (or a step of one) lands, the decision it made gets an ADR in
the same PR or the next. The ADR links the PR and, for anything measured, the
bench result files (each carries a manifest of what produced it) and the
`bench.py compare` table, with every number, including the ones that went
against the decision. Cite only evidence that exists in the repo or on GitHub.
Decisions made before this rule are backfilled the same way, from the code
comments and `bench/results/logs/observations.md` that recorded them.
