# 0033 - ACP live attachment

Status: accepted. 2026-11-02. Implements #155. Supersedes ADR 0032's refusal
of an already loaded session after a new initialize, and its assignment of
live attachment to the host runtime.

## Context

Deck keeps `construct acp` alive while its window is closed. A reopened window
has no transcript or pending request table. Deck ADR 0002 assigns live replay
to construct so the holder only frames lines. Crash recovery from ADR 0032
cannot serve this case: the agent still owns the session and its tools may
still be running or waiting for permission.

Reading the latest journal during attach is insufficient. `Writer::emit`
appends before ACP consumes the event, and text deltas are not journaled.
Combining a fresh journal read with a live text buffer can therefore replay
a commit and later deliver its queued text again.

## Decision

The user-mode ACP server advertises
`agentCapabilities._meta.construct.liveAttach: true`. Each `initialize`
starts a new attachment on the same stream. The initialize response is the
visible boundary: FIFO output puts any response already queued before it,
and no response from an earlier attachment can follow it. Delegated children
do not advertise live attachment or the turn extension.

The server actor owns the attachment number, transcript projection, prompt
queue and unanswered permission questions. Turn tasks send events, questions
and completion to this actor. The turn relay drains available events before
forwarding a question, so a permission request cannot overtake its tool start.
Completion follows all events, and the actor then starts the next queued prompt.
A session instance number prevents commands from a closed instance from changing
a later load of the same session ID.

Each session's transcript is seeded once from the journal and advanced at the
same point as ACP delivery. It retains display content rather than complete
model requests and model-facing conversation messages. Assistant commits
replace their partial text.
If a stream ends without a commit, its displayed text remains in the live
projection. The crash and live paths share this projector; unresolved tool
calls are failed after crash recovery, pending before execution in a live
session, or in progress after a live tool start. Compaction does not shorten
the client's transcript.

After a new initialize, `session/load` of an existing session validates its
original directory and serves the projection without taking the agent lock
or reopening its journal writer. It replays the transcript, then partial text
and outstanding tool calls, reissues unanswered questions, reports turn state
and answers with `_meta.construct.turn`. These sends are one synchronous actor
operation. Events produced meanwhile queue behind it and continue the replay
without gaps or duplicates. Transcript delivery waits for the session to load
on the new attachment; its projection continues advancing while detached.
Disk loads still use normal agent construction and crash recovery. Their load
response also includes turn state. `session/resume` remains available, though
Deck always uses load to rebuild its view.

`_construct/turn` reports `{ sessionId, turn, state, stopReason? }` on every
state change and attachment. `turn` is a per-session unsigned integer,
starting at zero and increasing for each prompt that begins. A recovered
session seeds this counter from journaled turn starts. State is `working`,
`waiting` while permission questions remain, or `idle`. An initial idle has
no stop reason. A completed live turn retains `end_turn`, `cancelled`,
`max_turn_requests`, or `error`. The load metadata contains the same object
as the notification. A superseded prompt finishes with the turn notification
alone, without its old request response.

Questions retain their permission payload and oneshot waiter under a stable
internal key. Initialize retires all wire IDs immediately, including before
load. Load assigns new, never reused wire IDs to unanswered questions while
keeping their payloads and question identities unchanged. A retired answer does
nothing. Answered questions are removed before their
waiter wakes. Cancellation removes by internal key, so replacing a wire ID
cannot leak a waiter. EOF cancels turns and continues draining their output.

## Consequences

Live replay keeps a display transcript in memory for each loaded session.
It avoids changes to the core event vocabulary or journal sequencing.
Attach can briefly delay output for other sessions while it queues a large
replay, but never waits for model or tool work. Process loss still discards
uncommitted text and live permission waiters; the durable recovery contract
is unchanged.

A shared connection mutex could enforce the same ordering, but would move
server-owned state behind another synchronized interface. A journal checkpoint
could avoid the display copy, but would change event transport below ACP for
one consumer. The actor keeps attachment policy at the existing process edge.

## Evidence

Issue #155 and strf0x1/deck ADR 0002. `construct-dev/tests/acp_live.rs` drives
one real ACP process through repeated initialization during streaming, a
permission wait, a running tool and detached completion. It checks exact
replayed text plus continuation, replaced question IDs, old response
suppression with reused client IDs, turn metadata and journal outcomes.
`construct-dev/tests/acp.rs` retains the crash-path load, ownership, directory,
resume and close tests from ADR 0032. The live tests also cover multiple
questions, cancellation after reissue, answered questions, EOF, queued prompts
and partial text surviving cancellation and a later turn.
