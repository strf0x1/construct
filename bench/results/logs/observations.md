# Observations not captured in result JSON

- RETRACTED: I first reported that opencode "stalls on first run in a fresh home".
  It was a bench bug: run() didn't close stdin, and opencode's `run` command reads
  piped stdin as part of the message and waits for EOF; the inherited pipe never
  closed. Verified 2026-10-19 on the same home: stdin=/dev/null -> session created
  at once; stdin=open pipe -> stops after `init`. Fixed (stdin=DEVNULL for every
  harness). opencode numbers from completed runs are unaffected; stall counts void.
- construct's tool latency first measured ~44ms/step because the bench endpoint
  didn't set TCP_NODELAY (the response tail waited on delayed ACKs). Hermes and
  opencode were unaffected by the fix (their numbers didn't move); construct's
  dropped to ~1ms. Real endpoints set TCP_NODELAY, but construct's client is the
  one that showed sensitivity to it.
- hermes 0.19.0: `hermes -z` (one-shot) ignores agent.max_turns and HERMES_MAX_ITERATIONS; it is hard-capped at 90 tool iterations (AIAgent default). The 120-step memory task therefore can't finish in Hermes headless mode; the reported memory run uses 80 steps so all three complete.
- INVALIDATED: the first real-task baseline (results/invalid/task-20261019-205214.json)
  ran with a stale $PWD (the bench passed the parent's environment unchanged).
  opencode takes its working directory from $PWD, so its prompt said "Working
  directory: /home/brandon/git/construct" and it loaded construct's own AGENTS.md;
  in one run it ran `cargo test` in the construct repo for ~850s. Hermes and
  construct use getcwd() and were unaffected. The bench now sets PWD per run.

## Jev eval (2026-10-20, bench/jev/)

Offline routing/gate eval: 47 tuned + 18 held-out routing cases, 18 + 8 gate
cases, 14-skill library, 3 MCP servers. `python3 bench/jev/eval.py [--set holdout]`.

- jev-1.13.0 as first designed (one noul per tool group) was worse than the
  local keyword router on groups: 12/47 vs 33/47, 50 false positives. Nouls
  read literally and were uncalibrated per question: `mcp:github` came back
  >0.6 for nearly any coding request ("rename Config to Settings" 0.90),
  `history` fired on any time word ("thanks!" 0.62, "yesterday's outage" 0.67).
- Rephrased as two choices (what to access: none/web/history/mcp:*; what the
  request is doing: task/preference/skill-edit) groups went to 40/47 (held-out
  15/18 vs local 11/18). Threshold left at 0.5; 0.6 scored 41/47 but that is
  tuning on the eval set.
- Jev favours the first option of a choice. Measured on the gate question:
  putting "preference" first instead of "task" moved ambiguous cases by
  +0.2-0.35 ("I want the parser to accept trailing commas" 0.24 -> 0.58);
  clear cases didn't move. construct's choices were going out alphabetically
  (serde_json without preserve_order), which put "preference" first. Fixed:
  preserve_order, conservative option first. Gate: 15/18 -> 18/18 on the
  tuned set, held-out 7/8 (local 5/8).
- Skills: Jev 42/47 (held-out 16/18) vs local 35/47 (14/18). Local misses
  every paraphrase ("get a shell on the prod ingest box" -> aws-ssm-connect).
  Jev's misses are suggesting a plausible-but-wrong skill (≥0.6 "clear
  favourite" rule): "please always run clippy" -> rust-release 0.81.
- Latency: route p50 ~100ms, p95 ~160-220ms, cold first call ~200-320ms;
  gate p50 ~180ms. Now pre-connected at startup. Stable across 3 repeats on
  43-45/47 and 18/18.
- First Jev-vs-local A/B attempt (2026-10-20) void: ~/.config/construct-bench.key
  had been overwritten with the Jev key (byte-identical files), so every
  upstream request got 401 and ~30 requests carried the Jev key to
  openrouter.ai. bench.py now refuses a non-`sk-or-` key or one equal to the
  Jev key before forwarding anything.

## Jev vs local A/B, real model (2026-10-20, results/task-20261020-103014.json)

minimax/minimax-m3.1-flash:free, warm-up + 5 interleaved rounds, both homes
seeded with the 14-skill library (bench/jev/library.json), binary d70d542.

- Routing was identical: all 18 turns in both variants routed to core tools
  only, no skill suggestions. The stats task needs none of the library, and
  both routers said so. Jev cost ~100ms/turn and changed nothing here.
- Token/request gap (median 67K vs 52K prompt tokens, 18 vs 15 requests) is
  model path variance, not routing: identical routes and tool lists, and the
  local variant had its own 25-request round. Treat as noise until more rounds.
- Peak RSS 42 vs 29MB is bench-measured over the process tree (includes the
  python test/doctest processes the model runs). construct alone with Jev vs
  local on a scripted endpoint: 16.1MB vs 16.1MB, 3 runs each.
- Learning: local gate never fired (6/6 None). Jev gate fired once
  (workaround=0.56, a README doctest failing inside a markdown fence) and the
  review created `python-lib-tdd`: trigger lines quoting the bench prompts,
  generic steps, and one real pitfall. A one-session, task-shaped skill: the
  recurrence problem the learner redesign targets. One Jev gate call failed
  (network) and fell back to local.

## Correction: strict nouls beat the choice design (2026-10-20)

Earlier I concluded nouls "read literally" and switched group routing to
choices. That was mostly bad wording. thruwire/foreman phrases nouls strictly
("Does the work explicitly require ...", "Answer no when ... insufficient");
the same pattern here ("Does `request` explicitly require <group>? Answer no
if it can be done with only the local files and a shell, or if the evidence is
insufficient.") scores groups 43/47 (held-out 16/18) vs the two-choice design
40/47 (15/18) and loose nouls 12/47. Nouls also have no option-order bias and
allow several groups. The router now uses strict nouls; the learning gate keeps
its task-first choice (18/18, 7/8). Latency unchanged (~110ms p50).
- Routing A/B attempt 1 (2026-10-20) void: the no-skill agent searched outside its work dir, read bench/bench.py (which held the skill text and the key id) and sibling runs' homes. Key id is now random per run and absent from the repo history. Also: nothing stops a construct agent from reading anywhere on disk; noted for the safety model.
