# 0003 — Jev routing asks strict nouls, not choices

Status: accepted, 2026-10-20 (backfilled 2026-10-21).

## Context

Each turn the router decides which optional tool groups (web, history, memory,
skill-write) and which MCP servers to put in front of the model, so the first
request stays small. With a TypeSafe key, Jev answers this as one request of
typed questions: a **noul** (a yes/no probability) or a **choice** (one of N
options). Three designs were scored with `bench/jev/eval.py` on 47 tuned and 18
held-out routing cases (14-skill library, 3 MCP servers):

1. One loosely worded noul per group ("would X help with `request`?").
2. Two choices: what to access (none/web/history/mcp:*) and what the request is
   doing (task/preference/skill-edit).
3. One strict noul per group: "Does `request` explicitly require <group>?
   Answer no if it can be done with only the local files and a shell, or if the
   evidence is insufficient." The wording is borrowed from thruwire/foreman.

Jev also favours the first option of a choice, so option order is a hidden
input to any choice question.

## Decision

Group and MCP-server routing uses strict nouls: each phrased "explicitly
require …", each with an explicit "answer no" clause (`NO_UNLESS`, `Q_*` in
`crates/construct/src/router.rs`). Skills stay a single choice over the library.
The learning gate keeps its choice question, with the conservative option
(`task`) listed first; choices must be sent in declared order (serde_json
`preserve_order`), never alphabetically.

A change to router or gate wording is scored with `python3 bench/jev/eval.py`
and `--set holdout` before and after.

## Consequences

- Nouls have no option-order bias and allow several groups at once.
- Wording is load-bearing. A loose rewrite of a `Q_*` string can silently
  collapse accuracy, so those strings are not "cleaned up" without an eval.
- The strict wording is tuned on this eval set; the held-out set is the check
  against overfitting it.
- Revisit if the Jev model version changes: the first result below was for
  jev-1.13.0.

## Evidence

Offline eval, from `bench/results/logs/observations.md` ("Jev eval" and
"Correction: strict nouls beat the choice design"); result files are
`bench/jev/results/eval-*.json`; cases in `bench/jev/routes*.jsonl`,
`gates*.jsonl`. Commits: `d70d542` (choices, option order), `ea09df1` (strict
nouls). Both are commits on main (no PR number recorded here).

| Group routing design | tuned (47) | held-out (18) | notes |
|---|---|---|---|
| loose noul per group | 12/47 | – | local keyword router 33/47 (held-out 11/18); 50 false positives |
| two choices | 40/47 | 15/18 | threshold 0.6 scored 41/47, left at 0.5 as tuning on the eval set |
| **strict noul per group** | **43/47** | **16/18** | latency unchanged, p50 ~110 ms |

Other numbers from the same notes:

- Loose nouls misfired on nearly everything: `mcp:github` above 0.6 for almost
  any coding request ("rename Config to Settings" 0.90), `history` on any time
  word ("thanks!" 0.62).
- Option order: moving "preference" ahead of "task" in the gate question raised
  ambiguous cases by +0.2–0.35 ("I want the parser to accept trailing commas"
  0.24 → 0.58); clear cases did not move. Putting the conservative option first
  took the gate from 15/18 to 18/18 on the tuned set (held-out 7/8, local 5/8).
- Skills (a choice, unchanged): Jev 42/47 (held-out 16/18) vs local 35/47
  (14/18). Jev's misses suggest a plausible wrong skill at ≥ 0.6.
- Latency: route p50 ~100 ms, p95 ~160–220 ms; gate p50 ~180 ms.
- Downstream, with the choice-era router: `bench/results/routing-20261020-111915.json`
  (Jev suggested the right skill in 6/6 runs; median 17 requests vs 28 for the
  local router). That run predates the strict-noul change and does not measure it.
