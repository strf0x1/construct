# tally

Household spending from a plain-text ledger.

```
2026-03-04 | food | 61.10 | groceries
```

One entry per line: `date | category | amount | memo`. Refunds are negative.

## Use

```sh
python3 -m tally report                 # totals per category
python3 -m tally balance --from 2026-03-01 --to 2026-03-31
python3 -m tally categories
python3 -m tally --ledger ~/money.ledger report
```

(Run from `src/`, or install with `pip install -e .`.)
