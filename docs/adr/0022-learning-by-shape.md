# 0022 — Learning keeps lessons by shape; `hybrid` is the default

Status: accepted. 2026-10-28.

## Context

The learner (`[learn] strategy = "skills"`, today's design) runs a gate, keeps
an observation ledger, and writes a skill only once a problem recurs. Two
curricula were built to judge it against alternatives with planted lessons
whose ground truth is known (#94, #102):

- **`learn-tally`:** six sessions in one home, with lessons of four kinds. A
  value only the user knows (a team seed), a stale documented test command, and
  two conventions the user enforces (a changelog line, Decimal for money).
- **`learn-tally-long`:** ten sessions on the same repo, adding three more. A
  five-step release procedure the user walks through once, a test command that
  goes stale midway, and a credential the user pastes.

What they showed about `skills`:

- **Without a Jev key it learns nothing.** The local gate returned `None` in
  19 of 24 sessions where the user had just corrected the agent (issue #5).
- **With Jev it writes good procedure skills, but loses facts.** It stores
  `<team seed>`, not the value, because the memory prompt says never to save
  secrets, so the user repeats the seed every session.
- **Its skills were rarely reachable.** The prompt listed project skills, but
  the tool that loads them was only present when the router suggested one.

`playbook` (ACE-style bullets with helpful/harmful counts, always in that
repo's prompt) fixed the facts but failed the procedure. It crammed the release
into one bullet, and the 400-character cap cut off its last steps.

The shapes differ, and so should the stores:
- A fact, value, convention or pitfall is short, needed on every task in its
  repo, and triggered by where you are.
- A procedure is long, needed only sometimes, and triggered by what you do.

## Decision

1. **The default is `[learn] strategy = "hybrid"`** (`learn/hybrid.rs`). One
   reflection per session decides each lesson's shape:
   - **A fact, value, convention or pitfall** is a playbook bullet: one line,
     edited by deltas (tag, add, update, remove), never rewritten whole, in the
     system prompt wherever its repo is.
   - **A procedure for one kind of task** is a project skill, plus a pointer
     bullet the learner adds itself ("For cutting a tally release: load the
     `tally-release` skill first…"). Being in the repo is enough to know the
     procedure exists; no request has to mention it.
2. **Learning still earns its tokens.** Hybrid reflects only when
   `gate::evidence` finds something:
   - the usual gate (Jev or local) fires; or
   - the user sent a follow-up; or
   - a tool call failed, including output that shows an environment failure
     (a missing command or file) although the command exited 0. A piped
     `cmd | tail` hides the exit status, and that's how a stale command
     goes unnoticed; a failing test doesn't count; or
   - a skill was loaded.

   A smooth session costs no call (`a_session_with_nothing_to_learn_costs_no_review`).
3. **A bullet that proves harmful more than helpful, twice, is dropped.**
   Later sessions' outcomes label earlier lessons; over the cap, the least
   proven go first.
4. **Credentials never enter a learned store; project values do, verbatim.**
   Everything replayed into a prompt passes `guard::check`: memory, playbook
   bullets, workflow steps, and skills (create, edit, patch, support files).
   It refuses known key formats, URLs with a password, and secret-named
   `name=value` pairs. It keeps placeholders and values a user gave for a
   project (a seed, a port). The learner prompts say the same.
5. **Where a repo has project skills, the skill tool is on.** The prompt lists
   them, so the tool to load them is there whatever the router says.
6. **The other strategies stay, as arms.**
   - `skills` (gate, ledger, recurrence-gated skills, curator), `corrections`,
     `playbook` and `workflow` remain selectable. The bench and two e2e tests
     pin `skills` explicitly.
   - `workflow` is not recommended: its placeholder step erased the user-only
     seed on Haiku in 4 of 4 homes.
   - The curator still runs after a hybrid pass, for agent-written global skills.

## Consequences

- **What a corrected user gets.** Construct now learns from corrections without
  a Jev key, keeps exact project values, and can follow a taught procedure the
  next time it's needed.
- **The prompt grows in a repo where lessons accumulate.** By session 10 of
  learn-tally-long it was about 2.7k characters on Haiku and 3.8k on DeepSeek,
  versus about 1.1k with learning off. The project cap (24 bullets of at most
  400 characters) bounds it; pruning keeps it honest only when sessions tag
  bullets.
- **The observation ledger and recurrence gating don't run under the
  default.** Issue #6 (patches overfit) is addressed structurally: deltas plus
  counters replace whole rewrites. Issue #14 (harness lore saved as global
  memory) is not settled: a global bullet of that kind appeared in one
  DeepSeek playbook home.
- **What the evidence gate gives up.** A session with no follow-up, no failure
  and no loaded skill earns no "helpful" tags. Counters move only on sessions
  with evidence; Evidence below measures the cost.
- **What would make us revisit this:**
  - a curriculum where a procedure is needed every session (a skill's load call
    then costs more than a bullet);
  - a long-horizon run where prompt growth or stale bullets outweigh the gains;
  - a real-repo check (SWE-Bench-CL) that disagrees with the planted lessons.

## Evidence

PRs: #94 (curriculum, strategies, guard), #102 (hybrid, long curriculum, the
skill-tool fix). Every run is on Haiku 5.5 (`anthropic/claude-haiku-5.5`) and
DeepSeek v4.1 Flash 1017 (`deepseek/deepseek-v4.1-flash-1017`), with 4 rounds
per arm per model. Manifests are in each file. Cells are Haiku / DeepSeek.

**learn-tally, local gate** (`bench/results/curriculum-20261027-{013654,013814,015358,020926}.json`).
Corrections per later session:

| control | skills | corrections | playbook | workflow |
|---|---|---|---|---|
| 1.44 / 1.06 | 1.25 / 0.94 | 0.38 / 0.25 | 0.06 / 0.31 | 1.12 / 0.31 |

**learn-tally, `--jev`** (`-{030202,030303,032945,033032}.json`).
Corrections per later session:

| control | skills | corrections | playbook |
|---|---|---|---|
| 1.31 / 0.94 | 1.06 / 0.50 | 0.31 / 0.25 | 0.12 / 0.25 |

**learn-tally-long, `--jev`** (`bench/results/curriculum-20261028-{001840,001906,001959,002055}.json`
Haiku, `-{003345,003532,003927,004126}.json` DeepSeek):

| | control | skills | playbook | hybrid (ungated) |
|---|---|---|---|---|
| Corrections per session | 1.34 / 0.69 | 0.81 / 0.31 | 0.25 / 0.25 | **0.06 / 0.22** |
| Right first try | 0% / 38% | 25% / 69% | 75% / 75% | **94% / 78%** |
| Release probe right first try | 0/4 / 2/4 | 2/4 / 1/4 | 0/4 / 3/4 | **4/4 / 4/4** |
| Release probe requests | 10.3 / 21.3 | 9.8 / 27.8 | 8.5 / 17.3 | **5.8 / 14.5** |
| Stale `tools/test` hits after the move | 0 / 0.38 | 0.19 / 0.50 | 0.31 / 0.12 | 0.06 / 0.25 |
| Cost per session | $0.005 / $0.010 | $0.006 / $0.011 | $0.005 / $0.009 | $0.005 / $0.007 |
| Learner's share of spend | 0% / 0% | 22% / 19% | 19% / 11% | 22% / 22% |
| System prompt by the last session (chars) | 1.1k / 1.5k | 1.9k / 2.3k | 2.5k / 3.3k | 2.7k / 3.8k |
| Token in a learned store / in the repo | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

- Hidden feature tests: every arm 100%. Two DeepSeek `sync` sessions first
  scored as failures because of a scorer bug (the fake HTTP response returned a
  mock from `read()`); after the fix, 32/32 sync sessions pass. The files keep
  the original verdicts; `bench/results/logs/observations.md` records the re-score.
- Against the decision: on DeepSeek, playbook beat hybrid on stale hits
  (0.12 vs 0.25). Hybrid's prompt is the largest. No token was ever refused by
  the guard, because no learner tried to store one, so the guard is proven by
  unit tests only.

**The evidence gate, measured** on learn-tally-long with `--jev`: hybrid
ungated (#102's run, above) against two versions of the gate.
- **v1** counts a failure only when the call errors.
  Files: `bench/results/curriculum-20261028-{080107,080122,080124,080132}.json`
  (Haiku), `-{080455,080537,080718,080719}.json` (DeepSeek).
- **v2** also counts environment failures in output that exited 0: "No such
  file or directory", "command not found", "No module named", "Permission
  denied", "cannot access". It doesn't count failing tests.
  Files: `-{081609,081616,081623,081635}.json` (Haiku),
  `-{082043,082055,082138,082219}.json` (DeepSeek).

| | ungated | gate v1 | **gate v2 (decided)** |
|---|---|---|---|
| Corrections per session | 0.06 / 0.22 | 0.16 / 0.31 | 0.16 / 0.25 |
| Right first try | 94% / 78% | 84% / 72% | 84% / 75% |
| Release probe right first try | 4/4 / 4/4 | 3/4 / 3/4 | **4/4 / 4/4** |
| Stale `tools/test` hits after the move | 0.06 / 0.25 | 0.69 / 0.38 | 0.31 / 0.25 |
| Learner calls per session | 1.00 / 1.00 | 0.40 / 0.57 | 0.72 / 0.88 |
| Learner's share of spend | 22% / 22% | 11% / 8% | 20% / 16% |
| System prompt by the last session (chars) | 2.7k / 3.8k | 2.2k / 3.0k | 2.7k / 3.7k |

- **Why v1 failed.** On Haiku it was the worst arm of any run on the stale
  lesson. Haiku runs `./tools/test 2>&1 | tail`, so the missing command exits
  0. The gate saw no failure, no reflection ran, and the wrong bullet stayed.
  In 3 of 4 rounds the agent hit the stale command in every session from 6 to
  9 with zero failed calls.
- **What v2 costs, against the decision:**
  - It saves only 12–28% of learner calls, where v1 saved 43–60%.
  - On Haiku it needs more corrections than ungated (0.16 vs 0.06). Of
    those, 3 are in session 3, the release teaching session, where a
    correction is by design. The other 2 are changelog corrections in session
    1, which ungated never needed (n = 4).
  - The stale lesson on Haiku is 0.31 against ungated's 0.06.

  The gate is kept anyway: AGENTS.md's rule that learning earns its tokens
  holds, and the procedure, the lesson only hybrid learns, is unharmed.
- Every arm on both models: hidden feature tests 100%, the token in no store and
  not in the repo.
