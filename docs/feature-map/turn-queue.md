---
id: turn-queue
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/turn.rs, crates/construct/src/tui/view.rs, crates/construct/src/tui/input.rs, crates/construct/src/tui/worker.rs, crates/construct/src/agent/user_turn.rs]
verified_at: 2cc6584
verified_deps: b18c2b3bd3cc1215
---
# Queued messages

A message typed while a turn runs is not lost and not run in parallel: the input placeholder reads `queue a message… (esc to interrupt)`, the message waits, and it runs as its own turn when the running one ends. When the queue empties the placeholder returns to `ask construct anything… (/help)` and the activity line is empty.

## Sub-features

- `queue-placeholder` while a turn runs or a message waits, the prompt asks to queue and says esc interrupts.
- `queue-runs-next` each queued message runs as its own turn, in the order it was typed.
- `queue-settles` when the last queued turn ends the TUI is idle: ask-anything placeholder, no spinner, no activity line.

## How to get to it (user POV)

- With a turn running, type at the prompt and press enter.

## Done when

- The placeholder changes to `queue a message… (esc to interrupt)` when the first turn starts.
- Both messages appear in the transcript and the model is asked once per turn, with the queued text in its own request.
- After the last reply the placeholder is `ask construct anything… (/help)` and nothing spins.

## Driving it

Preconditions:

- Baseline state. The scripted model runs `sleep 2`, then answers `answer one` and `answer two`, one request per turn. Recipe: `.agents/skills/verify-construct/recipes/turn-queue`.

- **Queue.** Drive: `type first`, `key enter`, `wait "running shell"`, `wait "queue a message… (esc to interrupt)"`, `type second`, `key enter`. Expect: the busy placeholder and both messages in the transcript.
- **Settle.** Drive: `wait "answer one"`, `wait "answer two"`, `wait "ask construct anything… (/help)"`, `gone "queue a message"`. Expect: idle after both replies.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/turn-queue`. Expect: `ok: two queued turns ran; the third request carried the queued text; idle placeholder back, no spinner` — two `turn_start`/`turn_end` rows and two `user` rows in `journal.txt`, and `"content":"second"` in the last request.

## Gotchas

- The placeholder doubles as the busy marker; `queue a message…` means a turn or a queued prompt is in flight.
- The queued text lands in the third request body: the first two are the shell call and its reply.
