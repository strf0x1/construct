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
