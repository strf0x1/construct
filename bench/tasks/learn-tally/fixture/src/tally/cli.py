"""`tally` command line. `main(argv)` returns an exit code and prints to
stdout, so tests call it directly."""

import argparse
import sys
from datetime import date

from . import ledger, report

DEFAULT_LEDGER = "data/sample.ledger"


def _date(text):
    try:
        return date.fromisoformat(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a date: {text!r} (want YYYY-MM-DD)") from None


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="tally", description="Household spending from a plain-text ledger.")
    p.add_argument("--ledger", default=DEFAULT_LEDGER, help="ledger file (default: %(default)s)")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("report", help="totals per category")

    b = sub.add_parser("balance", help="spending between two dates")
    b.add_argument("--from", dest="start", type=_date)
    b.add_argument("--to", dest="end", type=_date)

    sub.add_parser("categories", help="list categories")
    return p


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        entries = ledger.load(args.ledger)
    except (OSError, ledger.LedgerError) as e:
        print(f"tally: {e}", file=sys.stderr)
        return 2
    if args.cmd == "report":
        lines = report.report(entries)
    elif args.cmd == "balance":
        lines = report.balance(entries, args.start, args.end)
    elif args.cmd == "categories":
        lines = ledger.categories(entries)
    else:  # argparse rejects anything else
        return 2
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
