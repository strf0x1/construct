# 0044 — Reasoning goes back under its own key; a stream has deadlines

Status: accepted, 2026-11-16. Amends 0012 (dialects): one non-standard
field can now reach an `openai`-dialect server. Answers items 1 and 5 of #47.

## Context

`Message::wire` dropped every assistant's reasoning, so a thinking model
lost its chain of thought between tool calls. For some providers this is a
hard failure, not only lost accuracy:

- DeepSeek, thinking mode (on by default): a request that carries `tools`
  must send back `reasoning_content` on every earlier assistant message, from
  every turn, or it gets a 400. Without `tools` the field is ignored.
- Kimi (K3 family): "pass the complete assistant message back as-is,
  including `reasoning_content`".
- OpenRouter: takes plain `reasoning` or `reasoning_details` passed back
  unmodified, either one. Signed or encrypted thinking (Anthropic, some
  Gemini) survives only in `reasoning_details`.
- Gemini 3 and later, called directly: the first function call of each step
  carries a thought signature, and a current-turn call sent back without it
  is rejected (4xx). Google's docs do not name the field in the
  OpenAI-compatible layer; it is a tool call's `extra_content`
  (`google.thought_signature`). With parallel calls only the first carries
  one.

ADR 0012 notes that some local servers reject unknown fields, so sending
`reasoning_content` to every endpoint was not safe.

Separately, the client set only a connect timeout. A provider that accepted
the request and then went silent hung the turn forever. One limit for the
whole stream would not work. A local llama.cpp can read a 256k prompt in
minutes of silence after the headers. A cloud stream that stops mid-answer
should fail within a couple of minutes.

## Decision

- **Echo what was sent.** `stream::Accumulator` records the delta key the
  reasoning arrived under (`reasoning_content` or `reasoning`) in
  `Message::reasoning_field`. It merges streamed `reasoning_details` pieces
  into blocks: a piece with the same `type`, `index` and `id` as the block
  before it extends that block's `text`/`summary`/`data`, a null never
  overwrites a value, and anything else starts a new block. The result is
  stored in `Message::reasoning_details`. `Message::wire` sends them back on
  assistant messages, for the whole history. `reasoning_details` goes alone
  when present, because it already holds the whole reasoning and sending the
  plain text beside it would send it twice. A server only ever gets a field
  it streamed itself.
- A tool call's `extra_content` is kept as it came, as
  `ToolCall::extra_content`, and sent back on that call. It is not keyed to
  Gemini or to a model name, so a new model that signs its calls needs
  nothing more. `reasoning = "drop"` leaves it alone: a missing signature
  is an error, not a saving.
- Journal rows from before `reasoning_field` existed have no key, so they
  are never echoed.
- `[provider] reasoning = "drop"` (also on `[providers.<name>]`) strips the
  fields in `Provider::shape` for a server that streams reasoning but
  rejects it on input. The default is `"echo"`.
- **Two deadlines per call.** The first bytes of the response body must
  arrive within `first_byte_timeout` (default 600 s), counted from the start
  of each attempt and covering both the headers and the body's first bytes.
  After that, every chunk must arrive within `idle_timeout` (default 120 s)
  of the one before; an SSE keep-alive comment counts as a chunk. Missing
  either deadline fails the call with an error that names the setting. It
  is not retried: a server that stayed silent for that long would usually
  stay silent again.
  For a non-success response, the complete error body must arrive within
  the original `first_byte_timeout` deadline before it can be classified for
  retry. A timeout or unreadable body fails the call with the HTTP status;
  a body-read failure retains its underlying cause.

## Consequences

- Echoed reasoning costs prompt tokens on every later request. Compaction
  sizes the real body (`agent/compaction.rs`), so it is counted, and the
  summary that replaces old rounds removes their reasoning as well.
- The request snapshots did not change: the scripted model in them sends no
  reasoning.
- A stall loses any partial output already streamed to the UI. Retrying
  from scratch when nothing has reached the UI is item 3 of #47, still open.
- If a provider wants reasoning only for the current turn, or under a key
  other than the one it streams, it gets a field on `Provider` and a
  `shape` edit, as 0012 prescribes.

## Evidence

- E2E tests in `crates/construct-dev/tests/stream.rs`. The second request
  carries `reasoning_content` and the resumed session does too. `drop`
  keeps it off the wire. A mid-stream stall and a silent endpoint each fail
  within budget with the setting named.
- Provider docs read 2026-11-16: api-docs.deepseek.com/guides/thinking_mode,
  openrouter.ai/docs/use-cases/reasoning-tokens,
  platform.kimi.ai/docs/guide/use-kimi-k2-thinking-model,
  aistudio.google.com/docs/thought-signatures and aistudio.google.com/docs/openai.
- Not covered: OpenAI direct (Chat Completions returns no reasoning; only
  the Responses API round-trips it) and Anthropic's own OpenAI-compatible
  endpoint (no thinking that can be sent back; use OpenRouter). Gemini's
  optional signature on a plain text reply is not kept.
- No tier-2 run yet. The accuracy effect on a thinking model is unmeasured.
