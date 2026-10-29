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

## Routing A/B and a key leak (2026-10-20, results/routing-20261020-111915.json)

Release task that needs one skill out of 15 (random signing key per run).
- construct-jev: Jev suggested stats-release (0.84-0.88) in 6/6 runs, the model
  loaded it on its first call, 5/5 success; median 17 requests, 64K prompt
  tokens, 53s.
- construct (local router): no suggestion in 6/6. The model searched the disk
  for the answer and found its own skill files in CONSTRUCT_HOME in 4/6 runs
  (loaded via enable_tools at call 14-21), 5/5 success; median 28 requests,
  493K prompt tokens, 215s. The bench's `suggested`/`loaded` fields read a
  truncated stdout; the numbers above come from session logs.
- LEAK: with no skill to go on, the local-router agent read outside its work
  dir in every run: env, ~/.claude/settings.json, ~/.codex/config.toml,
  ~/.bash_history, ~/.gnupg listing, home handoff notes, construct's repo,
  sibling bench runs. In construct/r2 it ran `cat` on
  ~/.config/construct-bench.key and construct-bench-jev.key: both keys went
  to OpenRouter and the free model's provider in a tool result. Two Jev runs
  (r3, r4) also wandered (~/.claude listing, ~/.config listing, sibling runs).
  The local copy of that session was redacted. Both keys need rotating.
  construct has no filesystem boundary for its tools; that's the gap.

## Learning loop, sandboxed (2026-10-20, results/loop-20261020-122108.json)

Repo whose tests need `./tools/gen-fixtures` first (nothing says so). Sessions
1-2: different tasks, user gives the hint in a follow-up. Session 3: third
task, no hint. Control: session 3 in a fresh home. 2 rounds. Sandboxed; key
scan clean; 0 tool calls outside the work dir in all 14 sessions (vs 1-25 per
run before the sandbox).
- The landmine is too easy: every session passed, control included; the model
  hits FileNotFoundError 1-3 times, finds tools/gen-fixtures and recovers.
  The bench's gen_first field was wrong (it counted reading test_stats.py as
  running tests); numbers below are recomputed from session logs.
- Session 3 landmine hits: jev r0 0, jev r1 1, local r0 1, local r1 2,
  control 1 and 2. Requests: jev 13/12, local 19/19, control 14/18.
- Local learner learned nothing in 2 rounds: its gate never saw the hint
  (no keyword matched "that's how this repo works"), and 3 errors fired it
  only for reviews that found nothing.
- Jev r0 ran the loop as designed: session 1 -> 3 ledger entries + a memory
  note; session 2 -> recurrence matched -> skill `golden-fixture-repo-tasks`;
  session 3 -> patched. Session 3 avoided the landmine via the memory note.
  The session-3 patch added task-specific advice (population vs sample
  variance): overfit creeping back in through patches.
- Jev r1 never saw a recurrence: in session 2 the model avoided the landmine
  and the user's repeated hint scored preference 0.37, under the gate.
- The router never suggested the learned skill: its description keys on a
  repo property ("tests read generated fixtures") that requests don't
  mention. Adding that phrase to the request -> Jev picks it at 1.0. Project
  knowledge is triggered by where you are, not what you ask.
- Seeded loop run 1 (2026-10-20) void after round 0: each session's work dir
  was a fresh `git init` with no remote, so sessions 1-3 got three different
  project kits and session 3 never saw what was learned. The Jev learner had
  saved the seed and the two-step test command to its kit; the local learner
  saved nothing (gate fired on 10-11 tool errors, review found nothing).
  Log kept in results/invalid/. Sessions of a round now share a git remote.
- Seeded loop run 2 (2026-10-20) void after round 0: the project kit id
  used the directory name (work0/work1/work2) as well as the remote hash, so
  each session still got its own kit, although the Jev learner saved the
  seed to the kit both times. Kits are now named from the normalized remote
  (host/owner/repo). Also found: an empty /tmp/.git (not created by any
  agent tool call; sandboxed commands can't write /tmp) made /tmp itself
  look like a repo; detection now requires .git/HEAD. /dev/shm was writable
  inside the sandbox (all of /dev was allowed); now only the device nodes a
  shell needs.
- In the earlier seeded run (round 0, local variant, no way to learn the
  seed) the agent tried hard to get it anyway: read /proc of other pids
  including the bench's (environ denied by Landlock), searched the web for the
  hash, listed ~/Documents (denied), and compiled a C SHA-256 brute-forcer it
  started with `setsid nohup`, so it outlived the session. Processes that
  leave construct's process group are not contained: see the issue.
- Seeded loop run 3 (2026-10-20) void: the laptop suspended 14:40-17:59 during round 0 session 1; wall times and the per-run timeout are wall-clock. Restarted under systemd-inhibit.
Seeded loop run 4 (2026-10-20) void: it died at 18:09, in round 0 session 1, when the Claude Code session that launched it exited and took its background jobs with it. Before the user's hint arrived, the session-1 agent was brute-forcing the seed from the sha256 check in test_stats.py (`.sweep2.py`: dictionary words and permutations, inside the sandbox). That is expected without the hint, but it burns the per-run timeout. Restarted as run 5 with setsid/nohup under systemd-inhibit.

## Seeded loop run 5 (2026-10-20, loop-20261020-211806.json) — first valid live result

minimax-m3.1-flash:free, 3 rounds, construct-jev vs control. The learning loop
worked end to end for the first time.

S3 (the measured, hintless session): jev 3/3 pass, control 0/3. S3 median
requests jev 12 vs control 45. S3 median outside-workdir probes jev 0 vs
control 13.

- r0: kit carried NOTES only; s3 passed in 9 req.
- r1, r2: recurrence gate fired (missing-fixtures seen in s1+s2), learner wrote
  project skill `golden-fixture-prerequisites` (seed, hash check, "ask the user,
  don't scan"); s3 loaded it and passed (12, 19 req).
- control fails every round, 43-61 req, 10-27 outside-workdir probes, all
  denied by the sandbox. Baseline cost of no memory.

Losses / caveats:
- Session 1 is expensive for jev too (56-84 req, 11-46 outside probes, up to 2M
  prompt tokens): it runs blind before the hint and tries to crack the 48-bit
  seed. Sandbox contains it but does not stop the waste -> motivates a
  stop-and-ask prompt rule ("if a task needs a value only the user can know,
  stop and ask; don't guess or search outside the workspace"). Measure with
  s1 outside_workdir + requests on a rerun.
- landmine_hits in this JSON is the OLD over-counting counter (counts reads of
  test source, not just failed runs). Fixed in bench.py this session: count
  only failing shell output. Corrected r1-s3 = 0 hits, not 4. pass/requests/
  outside_workdir are unaffected.
- A stray GLOBAL memory ("the sandbox only reaches the workspace, ask the
  user") was saved each round. Useful but it's tooling lore, not repo
  knowledge. Filed as a learner-scope issue rather than changed mid-run.

## 2026-10-21 12:12 note

Editor paired run (proposal 04 step 6): replace 40/40, hashline 37/40, hashonly 39/40 on the 8-task edit corpus; hashline arms cost +750-800 first-request tokens and +39-50% prompt tokens per run. All 3 hashline failures and the 1 hashonly failure are edit-shifted-block: the model ended inserted text in a single \n, which hashline reads as the terminator, dropping the intended blank line. line+hash rejected 4 stale anchors on edit-stale-reference, all re-read and recovered; hash-only followed the moved line first time. Issue #26 relocation not exercised: the model batched both duplicate-relocation edits in one call in 9/10 runs. Decision in docs/adr/0002. Earlier attempt the same day void: bench crashed after 15 runs on a half-written endpoint log line read by the new budget watcher (fixed in e442b69).

- `bench/results/task-20261021-121125.json`: model minimax/minimax-m3.1-flash:free; bench e442b69 dirty; replace 6ea1f93 dirty editor=replace router=local; hashline 6ea1f93 dirty editor=hashline anchor=line+hash router=local; hashonly 6ea1f93 dirty editor=hashline anchor=hash router=local

## 2026-10-21 17:28 note

Issue #26 paired run (stale anchors on duplicate lines): two-turn tasks edit-stale-duplicate (3-line block with a new copy added above between turns) and -gap (2-line block, as tall as the gap between copies). Runs with a silent wrong-line edit: replace 0/20, line+hash 0/10 + 6/10 (gap), hash-only 7/10 + 4/10; Fisher hashline vs hashonly on the issue's shape p=0.003, replace vs line+hash on gap p=0.011. Stale edits carry the final line's indent, so a wrong-line edit breaks the file; 4 hashline gap runs timed out investigating it. Replace never misplaced but investigated the unexplained change (median 138 s, 69k prompt tokens on gap). Pilot the same day (57/60, killed by a tool time limit, never saved) agreed in direction; it also exposed a no_wrong_line check that looked only at the branch indent (fixed in e34fbe1 before this run). compare's spread rule calls edit.rewrites 'same' at 0 vs 7/10: counts that are almost always 0/1 need a rate test. Proposal in docs/adr/0007.

- `bench/results/task-20261021-172610.json`: model minimax/minimax-m3.1-flash:free; bench e34fbe1; replace 6ea1f93 dirty editor=replace router=local; hashline 6ea1f93 dirty editor=hashline anchor=line+hash router=local; hashonly 6ea1f93 dirty editor=hashline anchor=hash router=local

## 2026-10-21 17:54 note

issue #9: sandbox helper supervises shell calls (subreaper + sweep). tools before (main 0131fe5) vs after: read/shell/wall all 'same'; shell p50 2.32 -> 2.36 ms

- `bench/results/tools-20261021-175219.json`: model None; bench 0131fe5; construct 0131fe5 editor=replace router=local
- `bench/results/tools-20261021-175425.json`: model None; bench 0131fe5 dirty; construct 0131fe5 dirty editor=replace router=local

## 2026-10-21 21:34 note

Issue #39 / ADR 0018: count salt measured. Stale pair: wrong-line runs line+hash 6/20 -> count 0/20 (p=0.020), hash-only 10/20 -> count 3/20 (p=0.041); salted arms reject the stale anchor instead (ANCHOR_MISMATCH / ANCHOR_NOT_FOUND), requests/tokens/wall same. hashonly-count confirmation rerun: 0/20 wrong-line, 16/20 stale rejected, 1 timeout (model reset the file to HEAD investigating). Corpus: no added rejections in either mode; edit-rename hash-only arms sent the line-above anchor from a fresh read in 6/10 runs (line+hash 0/10), a wrong-anchor failure no salt reaches. Pair session logs were lost to fresh_homes when the corpus started.

- `bench/results/task-20261021-201459.json`: model minimax/minimax-m3.1-flash:free; bench 4b6139a; hashline 4b6139a editor=hashline anchor=line+hash salt=rank router=local; hashline-count 4b6139a editor=hashline anchor=line+hash salt=count router=local; hashonly 4b6139a editor=hashline anchor=hash salt=rank router=local; hashonly-count 4b6139a editor=hashline anchor=hash salt=count router=local
- `bench/results/task-20261021-211455.json`: model minimax/minimax-m3.1-flash:free; bench 4b6139a dirty; hashline 4b6139a editor=hashline anchor=line+hash salt=rank router=local; hashline-count 4b6139a editor=hashline anchor=line+hash salt=count router=local; hashonly 4b6139a editor=hashline anchor=hash salt=rank router=local; hashonly-count 4b6139a editor=hashline anchor=hash salt=count router=local
- `bench/results/task-20261021-213233.json`: model minimax/minimax-m3.1-flash:free; bench 4b6139a dirty; hashonly-count 4b6139a editor=hashline anchor=hash salt=count router=local

## 2026-10-21 22:14 note

Proposal 04 step 5 (MCP tools as catalog entries), tier 1, construct only (opencode/hermes installs broken on this host). main 60f12bd vs branch 0602f19, compare: frame, tools, memory all 'same' on every metric; memory last_req_kb identical (1371 KB over 121 requests); size identical (binary 16373696 B, fresh home 37102 B). main's memory ran after the branch's (worktree lacked the gitignored research/src corpus until linked).

- `bench/results/frame-20261021-220342-main.json`: model None; bench 60f12bd dirty; construct 60f12bd editor=replace router=local
- `bench/results/frame-20261021-220607.json`: model None; bench 0602f19 dirty; construct 0602f19 editor=replace router=local
- `bench/results/memory-20261021-221122.json`: model None; bench 0602f19 dirty; construct 0602f19 editor=replace router=local
- `bench/results/memory-20261021-221442-main.json`: model None; bench 60f12bd dirty; construct 60f12bd editor=replace router=local
- `bench/results/size-20261021-220339-main.json`: model None; bench 60f12bd dirty; construct 60f12bd editor=replace router=local
- `bench/results/size-20261021-220605.json`: model None; bench 0602f19; construct 0602f19 editor=replace router=local
- `bench/results/tools-20261021-220544-main.json`: model None; bench 60f12bd dirty; construct 60f12bd editor=replace router=local
- `bench/results/tools-20261021-220809.json`: model None; bench 0602f19 dirty; construct 0602f19 editor=replace router=local

## 2026-10-21 23:38 note

Issue #51 (ADR 0012), tier 1, construct only (no memory corpus on host). main 3b16106 vs branch: size, frame same. tools: wall worse 73->111 ms with the mock's prefix-cache simulation on (233251); same with main's mock (233516) and with the simulation made opt-in (233750: read/shell/wall same). The cost was the debug mock re-serializing requests, not construct.

- `bench/results/size-20261021-232821-main.json`: model None; bench 3b16106; construct 3b16106 editor=replace router=local
- `bench/results/size-20261021-232841.json`: model None; bench d93039c; construct d93039c editor=replace router=local
- `bench/results/frame-20261021-232843-main.json`: model None; bench 3b16106 dirty; construct 3b16106 editor=replace router=local
- `bench/results/frame-20261021-232846.json`: model None; bench d93039c dirty; construct d93039c editor=replace router=local
- `bench/results/tools-20261021-233048-main.json`: model None; bench 3b16106 dirty; construct 3b16106 editor=replace router=local
- `bench/results/tools-20261021-233251.json`: model None; bench d93039c dirty; construct d93039c editor=replace router=local
- `bench/results/tools-20261021-233516.json`: model None; bench d93039c dirty; construct d93039c editor=replace router=local
- `bench/results/tools-20261021-233750.json`: model None; bench d93039c dirty; construct d93039c editor=replace router=local

## 2026-10-22 00:08 note

Issue #51 tier 2 (ADR 0012 accepted). Haiku 5.5: hit rate 0 -> 68% median, cost/run better on every task (stats-median -49%, release-signing -24%, edit-long-file -37%), success/requests/tokens same. DeepSeek V4.1 Flash control: all same (auto-caching, 71% -> 77%). Main's cache/cost backfilled from proxy records. Tool list grew in 2/24 runs (DeepSeek release-signing, mid-run).

- `bench/results/task-20261021-235623-main.json`: model anthropic/claude-haiku-5.5; bench 3b16106; construct 3b16106 editor=replace router=local
- `bench/results/task-20261021-235859.json`: model anthropic/claude-haiku-5.5; bench 8d0014d; construct 8d0014d editor=replace router=local
- `bench/results/task-20261022-000230-main.json`: model deepseek/deepseek-v4.1-flash; bench 3b16106 dirty; construct 3b16106 editor=replace router=local
- `bench/results/task-20261022-000644.json`: model deepseek/deepseek-v4.1-flash; bench 8d0014d dirty; construct 8d0014d editor=replace router=local

## 2026-10-22 05:37 note

Compaction baseline (proposal 01, ADR 0013): main 6/9 vs planner 9/9 at a 32k window, main's 3 compact-recall runs all died on fatal overflows; planner costs more tokens where main squeezed through at 0.97 fill, stubs re-run median 3/run; keep_results=4 same verdicts as 2 with lower medians. Earlier A/B attempt stopped: OpenRouter free provider returned 'Provider returned an empty response' in-stream (200), construct doesn't retry it (llm.rs, #47); bench now re-runs runs with provider failures (--invalid-retries).

- `bench/results/task-20261022-053503.json`: model minimax/minimax-m3.1-flash:free; bench 3b16106 dirty; main 3b16106 editor=replace router=local; new 3b16106 dirty editor=replace router=local; k4 3b16106 dirty editor=replace router=local

## 2026-10-25 20:42 note

#64 shell sandbox denials, Haiku 5.5, main vs branch arms, 5 rounds x 3 tasks. Branch: no filesystem-wide searches (main: 35 of 43 shell calls named paths outside the workspace, incl. find / and /run/media); asks the user instead. honest_blocked here is the OLD lost-file scorer (sandbox words only); rescored with the fixed one (asking the user counts), branch 5/5, main 4/5. shell.blocked/broad 'worse' is an artifact: main's build doesn't report them. first_request +164 bytes is the new prompt line.

- `bench/results/task-20261025-202458.json`: model anthropic/claude-haiku-5.5; bench 80f37aa; main 440d0f0 editor=replace router=local; branch 80f37aa editor=replace router=local

## 2026-10-25 20:42 note

#64 shell sandbox denials, openrouter/free (the user-testing model), main vs branch, fixed lost-file scorer. Reproduces the bug on main: 4/4 lost-file answers say 'not found anywhere on the machine'. Branch halves the hunting (find / or ~: 21 -> 10; requests 121 -> 83) and is honest 2/5 vs 1/5 (inconclusive at n=5). Journal shows every sandbox note and the stop firing; the free model ignores them (one more find / after the stop). shell.blocked/broad 'worse' is the same artifact as the Haiku run.

- `bench/results/task-20261025-204144.json`: model openrouter/free; bench 80f37aa dirty; main 440d0f0 editor=replace router=local; branch 80f37aa editor=replace router=local

## 2026-10-25 23:33 note

#64 with the hard stop, openrouter/free, 10 rounds, main vs branch arms. lost-file: false global absence ('not found anywhere on the machine/filesystem') in 6/7 main answers vs 1/7 branch; honest_blocked 20% -> 60% (p=0.17), no_false_absence 30% -> 60% (p=0.37); requests 11 -> 7.5, shell calls 6.5 -> 3.5; find / or ~ 39 -> 19 over all runs. The refusal fired in 1/10 lost-file runs. answered 70% both (free-model provider flakiness). shell.* 'worse' is the artifact: main's build doesn't report them; first_request +164 bytes is the prompt line.

- `bench/results/task-20261025-233242.json`: model openrouter/free; bench 5e221ce; main 440d0f0 editor=replace router=local; branch 1304d31 dirty editor=replace router=local

## 2026-10-27 18:06 note

Learning curriculum (learn-tally), 5 arms x 6 sessions x 4 rounds, Haiku 5.5 and DeepSeek v4.1 flash 1017, local gate; then 4 arms with --jev. Later tally sessions, corrections per session (Haiku/DeepSeek), local: control 1.44/1.06, skills 1.25/0.94, corrections 0.38/0.25, playbook 0.06/0.31, workflow 1.12/0.31; with Jev: control 1.31/0.94, skills 1.06/0.50, corrections 0.31/0.25, playbook 0.12/0.25. Local gate returned None in 19/24 skills-arm sessions (#5): it misses corrections phrased as how the repo works, and Haiku pipes test output through tail so failures exit 0. With Jev the skills learner fires and writes good procedure skills, but stores <team seed> not the value (memory's no-secrets rule), so the user still repeats it. Workflow's placeholders erased the seed on Haiku (0/4 homes). All arms 100% on hidden feature tests, no lore leaked into the other repo. Total about $3 of OpenRouter.

- `bench/results/curriculum-20261027-013654.json`: model anthropic/claude-haiku-5.5; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=local; skills 175f870 dirty editor=replace router=local; control 175f870 dirty editor=replace router=local; workflow 175f870 dirty editor=replace router=local; playbook 175f870 dirty editor=replace router=local
- `bench/results/curriculum-20261027-013814.json`: model anthropic/claude-haiku-5.5; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=local; skills 175f870 dirty editor=replace router=local; control 175f870 dirty editor=replace router=local; workflow 175f870 dirty editor=replace router=local; playbook 175f870 dirty editor=replace router=local
- `bench/results/curriculum-20261027-015358.json`: model deepseek/deepseek-v4.1-flash-1017; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=local; skills 175f870 dirty editor=replace router=local; control 175f870 dirty editor=replace router=local; workflow 175f870 dirty editor=replace router=local; playbook 175f870 dirty editor=replace router=local
- `bench/results/curriculum-20261027-020926.json`: model deepseek/deepseek-v4.1-flash-1017; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=local; skills 175f870 dirty editor=replace router=local; control 175f870 dirty editor=replace router=local; workflow 175f870 dirty editor=replace router=local; playbook 175f870 dirty editor=replace router=local
- `bench/results/curriculum-20261027-030202.json`: model anthropic/claude-haiku-5.5; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=auto; control 175f870 dirty editor=replace router=auto; skills 175f870 dirty editor=replace router=auto; playbook 175f870 dirty editor=replace router=auto
- `bench/results/curriculum-20261027-030303.json`: model anthropic/claude-haiku-5.5; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=auto; control 175f870 dirty editor=replace router=auto; skills 175f870 dirty editor=replace router=auto; playbook 175f870 dirty editor=replace router=auto
- `bench/results/curriculum-20261027-032945.json`: model deepseek/deepseek-v4.1-flash-1017; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=auto; control 175f870 dirty editor=replace router=auto; skills 175f870 dirty editor=replace router=auto; playbook 175f870 dirty editor=replace router=auto
- `bench/results/curriculum-20261027-033032.json`: model deepseek/deepseek-v4.1-flash-1017; bench 175f870 dirty; corrections 175f870 dirty editor=replace router=auto; control 175f870 dirty editor=replace router=auto; skills 175f870 dirty editor=replace router=auto; playbook 175f870 dirty editor=replace router=auto

## 2026-10-27 22:32 note

Playbook arm re-run after the credentials rule went into the playbook and workflow prompts (guard.rs): 1 round x 2 processes per model. Later tally sessions, corrections per session: Haiku 0.12, DeepSeek 0.12 (before: 0.06 / 0.31 local, 0.12 / 0.25 Jev), first try 88% on both. The literal team seed is still in the playbook in 4/4 homes: the rule against storing credentials didn't make the learner drop user-supplied project values. No refusals fired; the curriculum plants no credentials.

- `bench/results/curriculum-20261027-223112.json`: model anthropic/claude-haiku-5.5; bench 1f2004a dirty; playbook 1f2004a dirty editor=replace router=local
- `bench/results/curriculum-20261027-223121.json`: model anthropic/claude-haiku-5.5; bench 1f2004a dirty; playbook 1f2004a dirty editor=replace router=local
- `bench/results/curriculum-20261027-223119.json`: model deepseek/deepseek-v4.1-flash-1017; bench 1f2004a dirty; playbook 1f2004a dirty editor=replace router=local
- `bench/results/curriculum-20261027-223148.json`: model deepseek/deepseek-v4.1-flash-1017; bench 1f2004a dirty; playbook 1f2004a dirty editor=replace router=local

## 2026-10-28 00:42 note

learn-tally-long (10 sessions: adds a release procedure taught once and probed once, tools/test replaced by make test from s6, a token pasted in s8), --jev, 4 arms x 4 rounds, Haiku 5.5 and DeepSeek v4.1 flash 1017, ~$2.31. Haiku/DeepSeek. Corrections per later session: control 1.34/0.69, skills 0.81/0.31, playbook 0.25/0.25, hybrid 0.06/0.22. Release probe right first try: control 0/4 / 2/4, skills 2/4 / 1/4, playbook 0/4 / 3/4, hybrid 4/4 / 4/4; probe requests hybrid 5.8/14.5 vs playbook 8.5/17.3. Playbook's failure is mechanical: it crammed the release procedure into one bullet and the 400-char cap cut it mid-step 3, losing commit and tag; hybrid wrote a tally-release skill plus a pointer bullet and loaded it in the probe. Stale tools/test hits after the move: hybrid 0.06/0.25, playbook 0.31/0.12, skills 0.19/0.50; hybrid rewrote the bullet in place and tagged it harmful once. Token: no arm stored it anywhere or wrote it into the repo; the learners wrote 'supplied by the user at run time', so the guard never had to fire. Two DeepSeek sync sessions (hybrid, skills) scored failed because the hidden test's fake response returned a mock from read(); fixed (read() returns b'{}') and every sync work dir re-scored: 32/32 pass. The result files keep the original verdicts.

- `bench/results/curriculum-20261028-001840.json`: model anthropic/claude-haiku-5.5; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-001906.json`: model anthropic/claude-haiku-5.5; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-001959.json`: model anthropic/claude-haiku-5.5; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-002055.json`: model anthropic/claude-haiku-5.5; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-003345.json`: model deepseek/deepseek-v4.1-flash-1017; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-003532.json`: model deepseek/deepseek-v4.1-flash-1017; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-003927.json`: model deepseek/deepseek-v4.1-flash-1017; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto
- `bench/results/curriculum-20261028-004126.json`: model deepseek/deepseek-v4.1-flash-1017; bench d558e1c dirty; playbook d558e1c dirty editor=replace router=auto; control d558e1c dirty editor=replace router=auto; skills d558e1c dirty editor=replace router=auto; hybrid d558e1c dirty editor=replace router=auto

## 2026-10-28 08:23 note

Hybrid's evidence gate (ADR 0022), learn-tally-long, --jev, 4 rounds per model; Haiku/DeepSeek vs #102's ungated hybrid. Gate v1 (failed calls only): corrections 0.16/0.31 (ungated 0.06/0.22), release probe 3/4 / 3/4 (4/4 / 4/4), stale hits 0.69/0.38 (0.06/0.25), learner calls 0.40/0.57 per session. v1 missed the stale lesson because Haiku pipes ./tools/test through tail: exit 0, no failed call, no reflection; 3 of 4 rounds hit the stale command every session 6-9 with zero failed calls. Gate v2 also counts env-failure markers (No such file or directory, command not found, No module named, Permission denied, cannot access) in output that exited 0, not failing tests: corrections 0.16/0.25, release probe 4/4 / 4/4, stale 0.31/0.25, learner calls 0.72/0.88, learner share 20%/16% (ungated 22%/22%). v2 is the default.

- `bench/results/curriculum-20261028-080107.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080122.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080124.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080132.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080455.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080537.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080718.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-080719.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-081609.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-081616.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-081623.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-081635.json`: model anthropic/claude-haiku-5.5; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-082043.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-082055.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-082138.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
- `bench/results/curriculum-20261028-082219.json`: model deepseek/deepseek-v4.1-flash-1017; bench 8ca2b2d dirty; hybrid-gated2 8ca2b2d dirty editor=replace router=auto
