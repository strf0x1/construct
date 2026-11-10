# ledger

A small plain-text accounting tool: a Python package `ledger`, run as
`python3 -m ledger`. Standard library only.

## The journal file

```
; a comment
2026-01-05 Groceries
    expenses:food    42.50
    assets:cash     -42.50

2026-01-09 Salary
    assets:bank    1000
    income:salary
```

- A transaction is a header line `YYYY-MM-DD description` followed by its
  postings: lines that start with whitespace, each `account` then an optional
  `amount`. Account names contain no spaces.
- An amount is an optional `-`, digits, and optionally `.` and one or two
  digits. All arithmetic is exact decimal arithmetic, never floating point.
- At most one posting of a transaction may leave out its amount; it gets
  whatever makes the transaction sum to zero.
- Blank lines and lines starting with `;` are ignored.

## Errors

On the first problem, print one line to stderr and exit with status 1. `N` is
the 1-based line number of the transaction's header line.

- `error: line N: bad date` when the date is not a real calendar date
- `error: line N: transaction does not balance` when the amounts do not sum
  to zero (and none was left out)

## Commands

`python3 -m ledger balance FILE [PREFIX]`

One line per account that has a posting, sorted by account name, formatted as
`f"{amount:>12.2f}  {account}"`; then a line of 12 dashes; then the total of
the listed amounts as `f"{total:>12.2f}"`. With PREFIX, only accounts whose
name starts with it are listed (and totalled).

```
     1000.00  assets:bank
      -42.50  assets:cash
       42.50  expenses:food
    -1000.00  income:salary
------------
        0.00
```

`python3 -m ledger register FILE ACCOUNT`

One line per posting to exactly ACCOUNT, in file order, formatted as
`f"{date} {description:<20.20} {amount:>10.2f} {running:>10.2f}"`, where
`running` is the sum of the amounts printed so far.

```
2026-01-05 Groceries                -42.50     -42.50
```
