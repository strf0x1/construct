---
id: long-text
status: verified
surfaces: [tui]
depends: [crates/construct/src/tui/table.rs, crates/construct/src/tui/prompt.rs, crates/construct/src/tui/markdown.rs]
verified_at: 13a7141
---
# Long text wraps

Text the user has to read is wrapped to the screen, never cut with `…`. A markdown table cell too long for its column wraps onto more lines (a row is as tall as its tallest cell, and a faint rule separates the rows once any cell wraps); only column widths shrink, widest first. In the permission panel, a long path, shell command or tool call wraps too, because the user is deciding on exactly the part a cut would hide. Those panel rows share a budget sized to the screen so the four choices always stay visible; past it a row says `… N more rows`.

Cut on purpose, and not covered here: one-row widgets (tool header detail, the model picker, the slash palette, the status line, the delegate dock and peek) and tool output previews, whose lines are still cut to the width (`… N more lines` is their line cap).

## Sub-features

- `long-text-table` a long table cell wraps, keeping every word, with a rule between rows.
- `long-text-prompt` the permission panel shows a path, command or tool call whole, within a row budget that leaves the choices on screen.

## How to get to it (user POV)

- Make construct print a markdown table with a long cell, or trigger a permission prompt for a long path or command, on a narrow terminal (60 columns here).

## Done when

- The panel shows the whole path across several rows with no `…`, and the four choices are on screen.
- The table's long cell reads in full across rows under its column, with no `…` in any table row.

## Driving it

Preconditions:

- Baseline state; `setup.sh` writes a file in a deeply nested directory under `{outside}`. Model: read it, then reply with a table whose second row has a long cell. Recipe: `.agents/skills/verify-construct/recipes/long-text`.

- **Narrow.** Drive: `resize 60 30`. Expect: a 60-column screen, where both texts are wider than their space.
- **Panel.** Drive: `type read it`, `key enter`, `wait "PERMISSION" 8000`, `sleep 600`, `snap prompt`, `image {evidence}/prompt.png`. Expect: the path on `▸` rows, continued on the rows below, with the choices under it.
- **Table.** Drive: `key enter`, `wait "Docs" 5000`, `settle 300 5000`, `snap --style table`, `image {evidence}/table.png`. Expect: the long cell on three rows under `How it is enforced`.
- **Second view.** The recipe's `check.sh`. Drive: `.agents/skills/verify-construct/scripts/drive.sh .agents/skills/verify-construct/recipes/long-text`. Expect: `ok: the whole path and the whole table cell are on screen` — the panel rows and the table rows, frame and padding stripped and joined, contain the full path and the full cell text.

## Gotchas

- A wrap lands anywhere, so `expect` on a phrase is fragile; `check.sh` joins the rows without spaces and matches that.
- Unit tests pin the wrapping itself (`tui/table.rs`, `tui/prompt.rs`); this recipe proves it reaches the screen.
- The panel's intro line and choice rows are not wrapped and need about 56 columns.
