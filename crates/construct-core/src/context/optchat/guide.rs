//! The fixed guide the turn model gets under `optchat` (ADR 0043, "What the
//! model is told", item A): what `<chat>` holds, what the lines mean, and when
//! to `zoom`.
//!
//! The one choice a reader must know: `view::render` appends it to the system
//! message and never stores it, so the log, the journal and replay are
//! unchanged.
//!
//! Adapted from UniiChat's `prompt.txt` (Apache-2.0, `VictorTaelin/uniichat` at
//! `0500bf6`), without its product parts.

pub const GUIDE: &str = r#"# Your memory

<chat> holds this whole session before the current turn, oldest first,
one summary per line:

  id+n|text   the n entries from id on, summarized (newlines as spaces)

Each item is tagged with its kind: user (the user's words), assistant
(your replies and tool calls) or tool (their results). A short entry is
its own line, word for word. Lines merge in pairs as the session grows,
so recent lines cover one entry each and older lines cover more. No
entry before this turn appears in full.

The latest mention of a thing in <chat> is the truth: a later decision
replaces an earlier one.

When a line only mentions something you need (a decision, a value, what
you said, a past attempt, where a file is), zoom it until you have it
whole, before you act, guess or ask. For what was said or decided in
this session, <chat> and zoom come before searching files.

Summaries keep little of tool output. When a result holds something
that will matter later, say it in your reply."#;
