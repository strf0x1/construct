# 0045 — A resent prompt with its key never runs twice

Status: accepted, 2026-11-17. Implements #245 against contract 1.3.0 §12
(strf0x1/sprawlcommons-acp#9, released as `v1.3.0`). Builds on
0029 (recovery never resumes generation) and 0033 (live attach).

## Context

Deck runs `construct acp` in a VM. When construct, the VM or the connection
dies after a `session/prompt` was taken but before Deck saw its response,
Deck cannot tell whether the prompt ran. Live attach (0033) covers a window
that reconnects while construct is still running. It does not cover a
restart: `session/load` followed by the same prompt ran it a second time,
with any outside effects twice.

pi-durable solves this with a per-conversation `requestId` on submissions
(spec §6, checked at `earendil-works/pi@cead615`, findings on #245). ACP v2
standardizes the same idea as `PromptRequest.idempotencyKey` (schema
`v2.0.0-alpha.16`, "Retrying a prompt"). v2 is alpha, and adopting it means
a new prompt lifecycle on both sides, so v1 gets the key through the contract
instead, under v2's name and rules. Contract 2.0 can move it to v2's
top-level field without changing what it means.

The one choice the contract leaves to the agent is a recorded key arriving
with a different prompt: answer it as a retry (pi's choice), or refuse it.

## Decision

- **Advertised in user mode** as
  `agentCapabilities._meta["com.sprawlcommons/construct"].idempotentPrompts`.
  The current key only; the legacy key has no form of it.
- **The key is journaled with the prompt.** `Event::Submitted` holds a
  `Submission { text, idempotency_key }`. Its JSON is the old struct
  variant's plus an optional field, so old rows read unchanged and a row
  without a key writes exactly as before. The `submitted` row is committed
  before routing (0008, 0034), so a prompt and its key become durable
  together. A crash before that row means nothing ran and the resend runs
  once. A crash after it means the key is on record.
- **The server actor keeps a book of keys** (`acp/keys.rs`), seeded from the
  journal when a session is registered and advanced by the events it already
  relays. A lookup never takes the agent lock a running turn holds. For a
  request with a key:

  | The key is | construct |
  |---|---|
  | new | queues the prompt and journals the key on its `submitted` row |
  | on a queued prompt | queues nothing; this request gets that prompt's answer |
  | on the running prompt (from the moment it leaves the queue) | starts nothing; answered when the turn ends, with the same result |
  | on record with an outcome | answered at once with it |
  | on record without one, not running (a restart cut the turn off) | answered at once with `cancelled`; recovery started nothing (0029), and the client decides what to send next |
  | on record with different text | refused: `-32602`, `data.reason = "idempotency_key_conflict"`; the key keeps its first prompt |
  | malformed (not 1–255 characters, not a string) | refused with `-32602`, never run as a prompt without a key |

  A live turn's outcome is replaced by its actual result when it finishes, so
  every request for a key gets the same answer the first one did. A turn
  that fails before its `submitted` row leaves the key unused.
- **We refuse a reused key with different text** rather than answering it as
  a retry. Deck mints a fresh key for every prompt, so a mismatch can only be
  a client bug, and refusing makes it visible instead of silently dropping
  the second prompt. The contract and ACP v2 both allow this. "Different" is
  the prompt as construct reads it (`map::prompt_text`), the same text the
  `submitted` row holds.
- **Keys stay off the wire.** `Event::private` strips the key before an event
  goes out on the lossless channel. The standard `session/update` mapping,
  the replayed transcript and the load snapshot never carried it. It stays
  in the journal, where `journal_query` can read it like any other row.
- **Keys are kept as long as the session can be loaded**, as the contract
  asks: they are journal rows.

## Consequences

- Deck can resend a prompt after any crash without asking the user, once it
  pins 1.3.0 and sends a key. Until then nothing changes: no key, no lookup.
- A `cancelled` answer after a restart is honest but coarse. The client
  learns that the turn did not finish, not how far it got; the loaded
  transcript shows that.
- A resend whose original ended in an error gets the same error. Retrying a
  failed prompt takes a new key, which matches v2 ("a new key for a new
  submission").
- The book holds every key of a loaded session in memory: one short string
  and the prompt text per keyed prompt. That is negligible next to the
  transcript the server already keeps. Revisit if sessions grow to many
  thousands of prompts (#246 measures long sessions).
- The TUI, `run` and delegated children send no keys, and their turns are
  unchanged.
- Revisit when ACP v2 leaves alpha. The field moves to the top level and the
  prompt response becomes a `messageId`, which needs a stable id per user
  message in the journal. That is a separate decision.

## Evidence

- #245: the pi-durable source check, the proposed design, and the decision
  to refuse a reused key.
- Contract: strf0x1/sprawlcommons-acp#8 (issue) and #9 (draft PR, 1.3.0).
  `CONTRACT.lock` pins release `v1.3.0` (`dd86e24`); its vendored files hash
  the same as the candidate commit `b5014aa` the tests first ran against.
- e2e `acp_keys::a_prompt_resent_after_a_kill_is_answered_cancelled_and_never_runs_again`:
  kill during a shell call, reload, resend with the same key. The answer is
  `cancelled`, no model request is sent, the effect happened once, and the
  journal holds one `submitted` row with the key. A new key then runs.
- e2e `acp_keys::resends_wait_for_a_queued_or_running_prompt_and_a_finished_one_answers_at_once`:
  resends of a running and of a queued prompt are answered with their turns'
  results (two turns, two `submitted` rows for four requests). A resend of a
  finished prompt is answered without a model request. Different text is
  refused with `idempotency_key_conflict`. Three malformed keys are refused
  and journal nothing. The lossless channel carries `submitted` without
  either key.
- With deduplication switched off, both e2e tests fail: the resend after a
  kill runs again, and the live test journals four prompts instead of two.
- `acp_contract::idempotent_prompts_are_the_contracts` pins the constants to
  the vendored matrix. Unit tests in `event/submission.rs` pin the row shape
  with and without a key, and the stripped wire form.
- Golden request snapshots are unchanged: nothing the model sees moved.
