# Engineering conventions

These apply to every change in this repository. Reviewers reject changes that
don't follow them.

## Layout

- Library code lives in `inventory/`, tests in `tests/`, one test module per
  feature.
- No new dependencies; the standard library only.
- Keep functions short and documented: one docstring line saying what the
  function does, more only when the why isn't obvious.

## Errors

- Raise `inventory.errors.InventoryError(code, message)` for every business
  rule violation. Never raise a bare `ValueError` from library code.
- Error codes are `{{prefix}}-` followed by exactly three digits, e.g.
  `{{prefix}}-104`. Every function that raises uses its own code: a code
  identifies one function, so support can find the source from a log line.
  Codes from 900 up are reserved for missing-argument checks.
- Messages are lower case, no trailing period.

## Tests

- Test modules use `unittest`. The first line of every new test module is the
  suite marker comment `# suite: {{suite}}`; CI selects tests by it.
- One test method per behaviour; name it after the behaviour, not the
  function.

## Changelog

- Every user-visible change gets an entry in `CHANGELOG.md` under the heading
  `## Unreleased ({{ticket}})`. Create the heading if it isn't there.
  Entries are one line each, starting with `- `.

## Style

- Four-space indentation, double quotes, f-strings for formatting.
- Imports: standard library first, then the package, alphabetical within each.
- Prefer early returns over nested conditionals.
- Avoid abbreviations in public names (`quantity`, not `qty`).
- Constants are UPPER_CASE at module top; no mutable module-level state
  except the ledger.
- Comments explain why, not what. Remove commented-out code.
