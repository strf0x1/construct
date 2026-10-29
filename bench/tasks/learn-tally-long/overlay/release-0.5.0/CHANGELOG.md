# Changelog

## Unreleased

- `budget --limit CATEGORY=AMOUNT` flags categories over their limit.
- `money.split` puts the extra cents first, so the parts add up.

## 0.4.0

- `report --category NAME` reports one category.
- `export --csv` writes every entry as CSV.
- `summary --month YYYY-MM` reports one month.

## 0.3.0

- `balance --from/--to` takes inclusive dates.
- Ledger lines may omit the memo.

## 0.2.0

- `categories` command.
- Amounts accept a leading `$` and thousands commas.
