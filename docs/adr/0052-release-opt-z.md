# 0052 — Release builds optimize for size (opt-level z)

Status: accepted. 2026-11-19.

## Context

#271 asks for the smallest droppable binary. The release profile already
strips, fat-LTOs, single-codegen-units and panic-aborts; the remaining
compiler lever is `opt-level`. Round 1 of the issue measured `z` at −42%
standalone on the pre-split tree, and the ring swap (#286) plus the
env-only secrets gate (#287) have since landed. `z` trades speed for
size — the right trade only if the speed cost is invisible for a harness
that spends its life waiting on HTTPS. Tier 1 (`bench.py size frame
tools memory`, mock model) is the required evidence for startup, the TUI
loop, tool execution and request building.

## Decision

`[profile.release] opt-level = "z"` for the whole workspace. Measured on
the current tree (bf8951e + this one-line change, arm64-apple-darwin,
stripped, fat LTO, panic=abort):

| build | bytes | Δ |
|---|---|---|
| opt-level 3 | 14,671,856 | — |
| opt-level z | 8,775,232 | **−5,896,624 B (−40.2%)** |

Tier-1 A/B, 5 rounds a side, `bench.py compare` verdicts (files in
`bench/results/`, manifests included):

| metric | opt-3 | z | verdict |
|---|---|---|---|
| first_frame_ms | 14.6 [12.3–16.1] | 16.0 [11.2–19.0] | same |
| first_output_ms | 13.5 [11.6–15.2] | 15.3 [10.1–17.5] | same |
| ready_ms | 14.8 [12.5–16.3] | 16.1 [11.3–19.3] | same |
| read p50 (ms, n=75) | 2.18 [1.96–2.74] | 2.50 [2.20–2.84] | same |
| shell p50 (ms, n=75) | 19.6 [18.5–21.8] | 20.3 [19.0–22.2] | same |
| tools wall (s) | 0.395 | 0.402 | same |

Every delta sits inside the larger p10–p90 spread: no measurable local
overhead regression. The ~1.4 ms first-frame median shift is noise
against model latency measured in hundreds of ms and up. Error output
verified intact on the z binary (the round-3 `-Zfmt-debug` failure mode
does not apply to `z`), and a scripted-model turn runs clean.

## Consequences

- The shipped binary is ~8.8 MB with the keyring, ~6.9 MB env-only
  (#287) — the drop-and-run size story without touching code.
- Compute-hot paths would pay for this; construct has none today (SSE
  parsing and tool dispatch are I/O-bound). Revisit with a tier-1 A/B if
  a CPU-bound feature lands (local embeddings, heavy parsing).
- The `memory` tier-1 suite did not run: its RSS sampler reads `/proc`
  (Linux-only) and this decision was measured on macOS. Smaller text
  cannot raise RSS; re-run on Linux if a future change makes memory a
  question.
- Nightly levers (`build-std`, location-detail) remain off the table:
  single-digit percentages for a pinned toolchain (round 3).

## Evidence

`bench/results/size-20261119-230838.json` / `size-20261119-231525.json`,
`frame-20261119-230841.json` / `frame-20261119-231528.json`,
`tools-20261119-231045.json` / `tools-20261119-231733.json`,
verdicts in `compare-20261119-231744-1.json` (frame) and
`compare-20261119-231744-2.json` (tools). #271 rounds 1–3.
