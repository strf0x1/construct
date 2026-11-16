# csvtable

Turns CSV into an aligned Markdown table. Node.js, ES modules, no
dependencies.

## `src/csv.js`

`export function parse(text)` returns an array of rows, each an array of
strings.

- Fields are separated by commas; rows end with LF or CRLF.
- A field may be wrapped in double quotes. Inside quotes, `""` is one quote
  character, and commas and line breaks are part of the field.
- A line break at the very end of the text does not start another row. Empty
  text gives `[]`.

## `src/table.js`

`export function render(rows, align = [])` returns the table as one string,
lines joined with `\n`, no trailing newline. The first row is the header.

- In every cell, `|` becomes `\|` and a line break (LF or CRLF) becomes
  `<br>`. Widths are measured after that.
- A column is as wide as its longest cell, and at least 3.
- `align[i]` is `"l"`, `"r"`, `"c"` or missing. Cells, the header's included, are padded with spaces
  to the column width: on the right for `"l"` or missing, on the left for
  `"r"`, on both sides for `"c"` (the odd space goes on the right).
- The second line is the separator, each column filled to its width with `-`,
  with `:` in place of the first dash for `"l"`, the last for `"r"`, and both
  for `"c"`. A missing alignment is dashes only.
- Each line is `| ` + cells joined with ` | ` + ` |`.
- A row whose length differs from the header's throws
  `new Error("row N has M fields, expected K")`, N counting from 1 with the
  header as row 1.

```
| name  | qty |
| :---- | --: |
| apple |   3 |
| kiwi  |  12 |
```
(`render([["name","qty"],["apple","3"],["kiwi","12"]], ["l","r"])`)

## `bin/csvtable.js`

`node bin/csvtable.js [--align l,r,c] [FILE]` reads FILE, or standard input
when no file is given, and prints the table followed by a newline. `--align`
takes the alignments in column order, comma-separated; an empty item means
none (`--align ,r`).

On an error it prints `error: <message>` to stderr and exits with status 1.
