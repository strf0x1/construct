# Observations not captured in result JSON

- opencode 1.18.34: the first `opencode run` in a freshly created home hung before
  sending any model request (idle in epoll on one socket, log stops at `init`) in
  4 of 5 fresh homes created on 2026-10-19 (first manual run: killed at 180s;
  tools warm-up: ~10 min, then finished; tools round 0 of an aborted run: 0 requests;
  memory warm-up: killed by hand after ~22 min). Later runs in the same home were fine.
  Warm-ups absorb this; timed rounds don't include it.
- construct's tool latency first measured ~44ms/step because the bench endpoint
  didn't set TCP_NODELAY (the response tail waited on delayed ACKs). Hermes and
  opencode were unaffected by the fix (their numbers didn't move); construct's
  dropped to ~1ms. Real endpoints set TCP_NODELAY, but construct's client is the
  one that showed sensitivity to it.
- hermes 0.19.0: `hermes -z` (one-shot) ignores agent.max_turns and HERMES_MAX_ITERATIONS; it is hard-capped at 90 tool iterations (AIAgent default). The 120-step memory task therefore can't finish in Hermes headless mode; the reported memory run uses 80 steps so all three complete.
