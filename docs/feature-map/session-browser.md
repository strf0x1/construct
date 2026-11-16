---
id: session-browser
status: verified
surfaces: [tui, cli]
depends: [crates/construct/src/journal, crates/construct/src/agent/restore.rs, crates/construct/src/agent/user_turn.rs, crates/construct/src/tui/sessions, crates/construct/src/tui/restore.rs, crates/construct/src/tui/worker.rs, crates/construct/src/tui/turn.rs, crates/construct/src/tui/activity.rs, crates/construct/src/cli/resume.rs]
verified_at: e9abf9b
verified_deps: 6595262bdeeade5b
---
# Resume saved sessions

Find conversations from the exact current directory by their first prompt or a name you choose. Opening a saved conversation restores its full transcript and compacted model context, model choice and accounting. Interrupted tools have recorded results recovered or unknown outcomes explained; selecting history starts no model generation and retries no tool.

## Sub-features

- `session-browser-open` `/resume` and `construct --resume` show the same local archive without needing credentials. Startup escape exits; in-app escape returns to the conversation. Selecting the current session closes the browser.
- `session-browser-labels` the original first prompt labels a session; `/rename <title>` persists a manual name. Empty names are rejected and duplicates are allowed. Rows show conversation activity, user turns and current/interrupted/in-use markers. Unused heads and delegated children are excluded.
- `session-browser-search` typing searches names, complete first/latest prompts and IDs across the entire eligible archive. Arrows, ctrl+p/n, Page Up/Down and Home/End move the visible highlight; ctrl+u clears the query. Selected details include full first prompt, latest preview, directory, provider/model, UTC time and full ID; alt+Page Up/Down scrolls details. Narrow terminals keep compact previews and row metadata (`*` current, `!` interrupted, `busy` in use).
- `session-browser-switch` `/resume <id>` switches directly. Busy turns, queued prompts, permission requests and background jobs refuse switching immediately. A failed destination leaves the active conversation usable. Runtime generations keep old asynchronous events out of a newly opened conversation.
- `session-browser-context` switching appends to the selected ID, replays saved compaction for the model, and shows historical tools with ctrl+o. Saved model/provider wins unless explicitly overridden with `--model` or a model chosen after restoration fails. Missing providers/credentials require explicit selection.
- `session-browser-recovery` original prompts commit before routing. Reopening repairs unmatched turns and tool outcomes once. Known tool results are reconciled by call ID; unknown effects are explained without retrying. Lost background jobs are interrupted and pending completions wait for explicit continuation. Writer ownership is exclusive per session and released on process death.

## How to get to it (user POV)

- Launch `construct --resume`, or type `/resume` inside Construct.
- Use `construct --resume <id>` or `/resume <id>` for a known ID.
- Use `construct -c` / `--continue` to keep the existing newest-started session rule for this directory.
- Type `/rename <title>` to name the active session.

## Done when

- Both entry points find the same full eligible archive; cancellation and selecting the current session write nothing.
- A → B → A preserves each conversation's ID, complete transcript, model, accounting and effective compacted context. The next model request has no other session's messages and has valid tool pairing.
- Process termination during routing, streaming or tools recovers committed work without model generation, retrying tools, duplicating repairs or delivering background completions twice.
- A second process cannot write an owned session; after termination it can resume. Cross-directory IDs explain the original directory and relaunch command. Bare noninteractive resume and conflicting flags fail clearly.

## Driving it

Preconditions:

- Baseline build and a fresh home. Recipe: `.agents/skills/verify-construct/recipes/session-browser`; its setup writes a local model config and fixture.

- **Browse and switch.** Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/session-browser`. Expect: A and B are named, the browser filters and clears, narrow previews remain usable, and selecting A restores its earlier tool output. Evidence includes `browser.png`, `narrow.png`, `restored.png` and transcript snapshots.
- **Second view.** Drive: the recipe's `check.sh` (run automatically). Expect: exactly two heads and five requests; original prompts and names under each ID; restored A/B requests have their own model and history, valid call/result pairing and the original A tool output.
- **Startup, archive and model policy.** Drive: `cargo test -p construct-dev --test it sessions::`. Expect: cancellation without credentials creates no journal, an archive beyond one page remains searchable including Unicode and narrow screens, rename persists without changing activity, `-c` stays newest-started, and model overrides and unavailable providers follow explicit choices.
- **Forced termination.** Drive: `cargo test -p construct-dev --test it recovery::`. Expect: committed prompts survive routing/stream termination, known and unknown tool outcomes become correctly paired in the next request, repeated opens add no repairs, background delivery waits, and malformed history preserves the active conversation.
- **Compacted context.** Drive: `cargo test -p construct-dev --test it compaction::`. Expect: the next request keeps compacted results and valid tool pairing rather than expanding the transcript into the context window.

## Gotchas

- Recovery restores recorded conversation, not files, dead shell processes or provider caches. Unrecorded streaming tokens may be lost, and unknown tools may already have caused external effects.
- Older journals without original-prompt rows use their saved user text with legacy reminder stripping. Corrupt, unknown or incompatible rows prevent restoration instead of silently dropping history.
- Browser activity order intentionally differs from `-c`'s newest-started rule. Renaming, model selection, learning, opening and recovery repairs do not advance conversation activity.
- Enter on a palette command with a required argument completes it first; a second Enter on an empty `/rename` shows its usage.
