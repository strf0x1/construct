# 0021 — The home is private, tightened once, never loosened

Status: accepted. 2026-10-27.

## Context

`$CONSTRUCT_HOME` (default `~/.construct`) and everything under it were created
with the process umask, `755` on a typical machine. Other local users could
read memory (`memory/`), project kits (`projects/<id>/NOTES.md`,
`PLAYBOOK.json`, `WORKFLOWS.json`, skills) and the per-project journals
(`projects/<id>/journal.sqlite`), which hold full session transcripts,
including anything a user pasted. Found while adding the credential guard to
the learning stores (#94), which left directory modes out of scope because a
change touches every existing home.

Options for an existing, open home: only warn, tighten on every start, or
tighten once. Options for where: a chmod in every store that creates a
directory, a process umask, or the home directory alone.

## Decision

- **Only the home directory's own mode is managed**, in `home::make_private`,
  called once from `cli::run` before config is read. A directory under a `0700`
  home is unreachable by anyone else, so the stores (journal, project kits,
  memory, skills, spill, the learning ledger) keep creating with the default
  mode and need no chmods. No umask is set: that would also change the modes of
  files tools write in the user's workspace.
- **A new home is created `0700`** (its parents with the default mode).
- **An existing home is tightened once**: group and other bits are dropped
  (`mode & !0o077`), never added, so a stricter mode stays as it is. A one-line
  notice goes to stderr when that changes anything. A home owned by another
  account is not touched.
- **Then hands off.** A `.private` marker in the home records that it was
  handled; while it exists construct never changes the home's mode again. A
  user who shares the home on purpose widens it after the first run (or creates
  the marker first) and keeps that mode. Deleting the marker asks for one more
  pass.
- **Best effort.** A failure (read-only home, no permission) is ignored and the
  run goes on as before; a home construct cannot secure still works.
- Cost on the first-frame path: one `stat` of the marker once settled; no
  terminal queries, no secret or network work.

## Consequences

- Existing users' homes go private on their next start, without a prompt.
  Someone who relied on another account reading `~/.construct` must widen it
  once; construct will not undo that.
- Files and subdirectories keep their `644`/`755` modes. They are protected
  only by the home; a home the user opens again exposes them. Moving one store
  outside the home would need its own protection.
- Unix only, like the rest of the harness.
- Revisit if construct ever runs as a shared service account, or if a store
  must be readable by another user while the rest stays private.

## Evidence

- Unit tests in `crates/construct/src/home.rs`: new home `0700`, `755` →
  `700` once and a later `750` kept, a `500` home never widened.
- e2e `home_is_private_and_an_open_one_is_tightened_once` in
  `crates/construct-dev/tests/e2e.rs`: the real binary, a fresh home, an old
  `755` home without the marker, and a re-shared one.
- Golden request and screen snapshots unchanged.
