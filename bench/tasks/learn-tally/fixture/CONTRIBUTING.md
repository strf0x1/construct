# Contributing

- Keep modules small: parsing in `ledger.py`, arithmetic in `money.py`,
  text output in `report.py`, argument handling in `cli.py`.
- `cli.main(argv)` returns an exit code: 0 ok, 1 for a check that failed
  (e.g. over budget), 2 for bad input.
- New commands get a test in `tests/test_cli.py` that calls `cli.main`.
- Run the tests with `python -m unittest discover tests`.
