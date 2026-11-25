# 0071 — Remote sessions: ACP on the peer link, detach instead of cancel, resume by live attach

Status: accepted. 2026-11-25. Part of #273, #362. Builds on ADR 0033, 0035,
0045, 0069, 0070.

## Context

A joined peer (ADR 0070) has a link but no work. #362 makes the hub drive a
long-lived agent session on the peer and keep it through the things that go
wrong between two machines: a laptop sleeps, Wi-Fi drops, a tunnel idles
out, the hub is restarted or killed. Constraints:

1. `construct acp` treated input EOF as shutdown: every turn cancelled, every
   question closed. On a peer, a dropped WebSocket is the normal case, not a
   reason to stop work.
2. The server wrote frames to stdout only, with fd 1 claimed for the protocol
   (ADR 0035).
3. ADR 0055 removed construct's ACP client half.
4. The earlier plan for #273 assumed a custom `seq` resume protocol over the
   journal. Live attach (ADR 0033) and idempotent prompts (ADR 0045) already
   exist and are in the shared contract (1.3.0).
5. `session/new` and `session/load` require a `cwd` the peer's agent accepts
   (`acp/load.rs` refuses a load from another directory).

## Decision

- **The ACP server is transport-agnostic.** `acp::server::serve(cfg, paths,
  sources, input, out)` takes an input channel of `Input` and an output
  channel of lines. `run` is the stdio wrapper: it keeps ADR 0035's stdout
  claim and writer thread, and turns stdin into `Input::Line`. Stdin
  closing still closes the input, which still shuts down.
- **`Input::Detached` ends an attachment and cancels nothing.** It does what
  a new `initialize` already did (ADR 0033): bump the attachment number, so
  nothing more is written for the old client, and retire question wire ids.
  Sessions, running and queued turns, and unanswered questions stay. Only
  the input closing (SIGTERM or SIGINT on the peer, a revocation, a refusal)
  or `session/close` cancels.
- **The peer owns its `cwd`.** `construct peer` works in `--cwd` or the
  directory it started in, and reports it in `Hello.cwd` (an optional field on
  the peer channel, outside the ACP contract). The hub names exactly that
  directory in every `session/new` and `session/load`.
- **ACP rides only on accepted links.** `join::run` relays the `acp` channel
  only after `JoinAck { accepted: true }`, and drops lines the agent queued
  for an earlier link at that moment. The hub attaches only after it sent
  that `JoinAck` (at once for a pinned id, or on `approve`).
- **A joined peer redials.** After a lost link (or a hub that closed it on
  shutdown, or a failed redial) `construct peer` sends `Input::Detached`,
  waits with jittered backoff (250 ms doubling to 15 s), and dials again with
  the same key and no token. Refused, denied and revoked still end it.
  Surviving the peer's own process dying is #367.
- **The hub's ACP client is a pure state machine** (`construct-hub/src/remote.rs`),
  one per pinned peer, lines in and lines out:
  - Every attachment starts with `initialize` (asking for the reverse-DNS
    turn method), and once there is a session, `session/load`. That is
    ADR 0033's live attach: the peer replays the transcript, reissues
    unanswered questions with fresh ids, and reports
    `_com.sprawlcommons/construct/turn` in the load response. The hub
    replaces its view with the replay.
  - Nothing that arrives before the `initialize` response is read: it was
    for an attachment that ended. Request ids start at the clock in ms, so a
    restarted hub never reuses one an earlier hub sent the same peer.
  - The session is opened on the first prompt (`session/new`), not on
    connect. A load the peer answers with not-found (a wiped home) starts a
    new one.
  - **One prompt at a time.** Its text and a fresh `idempotencyKey` are
    written to the peer's registry row (`prompt`) before they are sent, along
    with `session` and the last `turn` number. After a reconnect or a hub
    restart, the hub resends that prompt with the same key. ADR 0045 then
    gives the answers: wait on the running prompt, answer at once from
    record, or `cancelled` if a peer restart cut it off. A reconnect never
    runs the prompt twice, and the hub sees one completion. The row's
    `prompt` is cleared when the answer arrives.
  - Permission requests from the peer wait in the hub until a human answers
    with `construct hub answer <peer> <option>` (the TUI is #365). Only the
    option ids the peer offered are accepted. A detach drops them; the load
    reissues them. An idle turn clears them.
- **No new ACP wire.** The hub uses `initialize`, `session/new`,
  `session/load`, `session/prompt` with contract 1.3.0's `idempotencyKey`,
  `session/cancel`, `session/request_permission`, `session/update` and the
  turn notification, all as the contract already defines them. Nothing
  starts in `sprawlcommons-acp`. `Hello.cwd` and `Input` are not ACP.
- **Control socket and CLI.** New ops `prompt`, `cancel`, `session` and
  `answer`, each answered with the session view (`view.rs`): attached or
  not, session id, `cwd`, last turn report, the prompt in flight, how the
  last one ended, the transcript rendered from kept updates (at most 4096),
  and open questions. CLI: `construct hub prompt <peer> <text…> [--wait]`,
  `cancel`, `session [--json]`, `answer <peer> <option> [--question n]`.

## Consequences

- The coordinator's `peer` tool (#366) and the TUI (#365) are clients of
  these ops. Neither needs its own ACP client or a resume cursor.
- One prompt at a time keeps the persisted state to one row field. A second
  prompt while one runs is refused with a reason. ACP's own queue stays
  available if that limit ever needs lifting.
- A hub that dies between choosing a key and writing the row sent nothing:
  the row is saved before the line goes out.
- While detached, the peer's turn notices and events are not delivered; the
  next load replays the display transcript (ADR 0033). Raw tool output beyond
  that display copy stays in the peer's journal.
- The hub keeps every pinned peer's last view in memory until it exits.
  Revoking a peer drops it.
- The peer redials forever while the hub is unreachable, including a peer
  revoked while offline, whose handshake fails silently. #367 settles exit
  codes for unit restarts.
- Not measured here: release binary size. The peer build already linked the
  ACP server (`construct acp` is in every build); the hub gains `remote.rs`
  and `view.rs`.

## Evidence

- e2e `crates/construct-dev/tests/peer_acp.rs`: real `construct hub` and
  `construct peer` processes, the peer with a scripted model.
  - `two_prompts_run_in_one_session_and_a_hub_killed_mid_turn_reattaches_without_running_it_twice`:
    - The first prompt opens the session in the peer's reported `cwd` and
      ends `end_turn`.
    - The second prompt starts a 5 s shell call. The hub is SIGKILLed once
      the call's side effect exists. `peers.json` still holds the session and
      the unanswered prompt with its key.
    - A new hub on the same address sees the peer redial. Its first view is
      attached, turn 2, `working`, with the replayed transcript.
    - The turn ends `end_turn`. The transcript holds both prompts, both
      answers and the completed tool call.
    - The peer's journal shows the turn ran once: one effect, three model
      requests, two `submitted` rows (the second with the hub's key), no
      `recovered` row, no interrupted turn. The row's `prompt` is gone.
  - `a_permission_question_reaches_the_hub_survives_a_restart_and_its_answer_reaches_the_peer`:
    a read outside the workspace asks. The hub shows the question
    (`waiting`). `hub stop` and a restart reissue the same question, still
    `waiting`, with no `tool_end` in the meantime. An option the peer did not
    offer is refused. `once` reaches the peer, the read returns the file, the
    turn ends `end_turn`, and there is one `submitted` row.
- Mutation: with `Input::Detached` shutting the server down instead, the
  kill test fails (the reattach never shows a working turn).
- Unit tests: `remote.rs` (a first prompt opens a session and its answer
  clears it; lines before the `initialize` response are ignored; a restarted
  hub loads and resends with the same key; question answers are checked
  against the offered ids and retired on detach; a lost session is reopened);
  `view.rs` (transcript rendering).
- The existing `acp`, `acp_live`, `acp_keys`, `acp_payloads`, `hub` and
  `join` e2e suites pass unchanged (43 tests together with the new ones).
