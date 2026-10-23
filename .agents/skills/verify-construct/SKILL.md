---
name: verify-construct
description: Drive construct's full-screen TUI like a user, with a scripted model, to prove what the screen shows and how it looks (text, colors, selection, images), plus the requests the model got and the journal rows written. Use after any change to the TUI, permission prompts, delegation, slash commands or the status line, to reproduce a UI bug, or whenever a change needs evidence beyond unit tests.
---

# Verify construct

construct is a TUI over a model endpoint. You can drive it end to end with no
real model and no human: `construct-dev serve` plays a scripted model,
`construct-dev drive` runs the real binary in a pty, sends keys, and reads the
screen back as text, as styled text, or as a PNG you can look at.

Read `docs/feature-map/README.md` and the feature file you are touching
before driving. Each feature file names its recipe here.

## Doctor

```bash
.agents/skills/verify-construct/scripts/doctor.sh
```

It prints `OK`, or says what is wrong: binaries missing, built from another
commit, or sources newer than the build. Run it before the first drive, after a
failed drive, and whenever the screen surprises you. A stale binary is the
usual cause of "my change did nothing".

## Launch, drive, capture: one command

```bash
.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/permission-prompt
```

It builds (skip with `VERIFY_NO_BUILD=1`), makes a scratch dir with its own
`CONSTRUCT_HOME`, work dir and `outside/` dir, starts a mock endpoint on a free
port, runs the recipe's drive script against `target/debug/construct`, and
prints the tail of the transcript, then `PASS <feature>` or `FAIL <feature>`.
There is no server to keep up: every run is its own instance, so runs never
share state and two can run side by side. Optional args: `cols rows`
(default 100 30).

A recipe is a directory named after the feature id:

| file | what |
|---|---|
| `model.json` | the scripted model: `{"steps":[{"call":"read","args":{"path":"{outside}/a.txt"}},{"text":"done"}]}`. One step per model request, in order, across the parent and any delegated children. |
| `drive.txt` | the drive script (commands below) |
| `setup.sh` | optional; runs first with `WORK`, `OUTSIDE`, `CHOME` (the construct home: put `config.toml` there) and `EVIDENCE` set |

`{work}`, `{outside}`, `{home}`, `{evidence}` in `model.json` and
`drive.txt` become those paths. For a one-off check, copy a recipe dir to your
scratch space, edit it, and pass that path.

Existing recipes, in `recipes/`: `permission-prompt`, `delegation`,
`slash-commands`, `status-line`.

## Drive script commands

```
wait <text> [ms]           block until <text> is on screen (default 10 s)
gone <text> [ms]           block until it is not
type <text>                type literal text
key <name>                 enter esc tab backspace up down left right pgup pgdn ctrl-<c> alt-enter
sleep <ms>
settle <quiet_ms> [ms]     wait until the screen stops changing
snap [label]               print the screen as text
snap --style [label]       print it with styled runs marked ⟦fg=#7aa2f7 bold⟧text⟦/⟧
expect <text>              fail unless <text> is on screen
expect-style <text> <attrs>  fail unless every glyph of <text> has <attrs>
image <path>               write the screen as .svg, or .png (needs rsvg-convert or magick)
resize <cols> <rows>
```

`<attrs>` for `expect-style`: `fg=#rrggbb`, `fg=<0-255>`, `fg=default`, the
same for `bg=`, and `bold dim italic underline inverse`, with `!` to negate
(`!bold`). Quote text that has spaces: `expect-style "Allow once" fg=#9ece6a`.

The TUI's colors are constants in `crates/construct/src/tui/theme.rs`
(ACCENT `#7aa2f7`, ACCENT2 `#bb9af7`, GREEN `#9ece6a`, RED `#f7768e`, YELLOW
`#e0af68`, CYAN `#7dcfff`, ORANGE `#ff9e64`, DIM `#6e7696`, FAINT `#464c66`);
the permission panel adds its selected-row fill `#2c3048` in `tui/prompt.rs`.
Assert colors from there, not from a screenshot.

**Look at it.** After an `image {evidence}/x.png`, open the PNG with your file
reader: you see the frame as a user would. Use it for layout and for anything
"looks wrong"; use `expect`/`expect-style` for what must stay true.

## Evidence

`.verify/evidence/<run-id>/<feature-id>/` (gitignored), kept after cleanup:

- `transcript.txt`: every `snap`, the failure screen if any, and `--metrics`
  timings as JSON.
- `requests.jsonl`: every request the mock received. This is what the model was
  told: tool results, permission outcomes, system prompt.
- `journal.txt`: the session journal (`seq | session | type | payload`):
  `access` rows hold permission decisions, `tool_end` rows hold results, and a
  child's rows sit under its own session id.
- any `image` output.

Prove a change from both sides: what the user sees (snap/image/expect-style)
**and** what the system did (requests.jsonl, journal.txt, files in `{work}`).
A "allowed once" notice is not proof that the read happened; the tool result
in `requests.jsonl` is. Report the run id and the recipe with the result.

## Cleanup

`drive.sh` kills only the mock it started (by its pid file) and deletes only
its scratch dir; evidence stays. `VERIFY_KEEP=1` keeps scratch for poking at
(`$CHOME/projects/*/journal.sqlite`). Never kill construct or construct-dev by
name: the user may be running their own.

## Gotchas

- **Prompts ignore keys for 450 ms** after they appear (so a keystroke already
  in flight can't answer one). `sleep 600` after `wait "PERMISSION"` before
  pressing anything, or the key is silently dropped.
- **The permission panel's border animates**, so `settle` never returns while
  one is open. Use `wait` and `sleep` there.
- **`expect-style` checks the first match on screen.** Your typed message is
  on screen too: `expect-style "scout" …` hits "ask scout". Pick text that only
  the element you mean contains (`"subagent · scout"`).
- **`NO_COLOR`**: agent shells often export it. The driver removes it from the
  child, so colors are real; `--env NO_COLOR=1` puts it back on purpose.
- Mock steps are consumed in request order by everyone: a delegated child's
  requests take the next steps, so write the parent's call, then the child's
  steps, then the parent's reply.
- A recipe never sees `~/.construct`: it runs with its own home, model
  `openrouter/free` from defaults, and a dummy key. Put any config it needs in
  `setup.sh`.
- The first frame waits on nothing, but the first `wait "ask construct"` still
  needs a few seconds on a cold build. Keep the 5000 ms budgets.

## Writing the test that stays

A recipe proves a change once. When the behavior must not regress, encode the
same script as an e2e test in `crates/construct-dev/tests/` (see
`permission.rs`: `Env::new`, `tui(&env, …)`, `play(&mut d, script)`, then assert
on `env.requests()` and `env.journal()`). `expect-style` works there too.

## Real terminal (optional, last mile)

The pty is a vt100 emulator, not a real terminal. For what only a real one
shows (font fallback, how ghostty or kitty handle synchronized updates or the
keyboard protocol, OSC 52 clipboard), run construct in a real window on the
desktop and look at it:

```bash
ghostty --class=verify.construct -e env CONSTRUCT_HOME=<scratch home> CONSTRUCT_BASE_URL=http://127.0.0.1:<mock port>/v1 OPENAI_API_KEY=x target/debug/construct &
# screenshot just that window (Hyprland): its geometry from hyprctl, then grim
g=$(hyprctl clients -j | jq -r '.[] | select(.class=="verify.construct") | "\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"')
grim -g "$g" .verify/evidence/<run-id>/<feature>/real-terminal.png
```

Start the mock with `scripts/mock.sh <dir> <model.json>` (it prints the
port). Sending keys to a real window needs a computer-use tool (CUA). Without
one, `wtype` types into whatever window has focus, which is the user's
desktop: ask before using it. Close the window you opened by its class
(`hyprctl dispatch closewindow class:verify.construct`), never other windows.
This tier is a manual check; it is not run in CI.
