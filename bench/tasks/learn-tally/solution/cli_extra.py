# A reference solution for every tally session at once, appended to cli.py by bench/test_bench.py: proof the hidden tests can pass.
import argparse as _ap
import csv as _csv
import re as _re
from decimal import Decimal as _D
from . import money as _money

_orig_build = build_parser

def build_parser():
    p = _orig_build()
    sub = next(a for a in p._actions if isinstance(a, _ap._SubParsersAction))
    sub.choices["report"].add_argument("--category")
    e = sub.add_parser("export"); e.add_argument("--csv", action="store_true")
    s = sub.add_parser("summary"); s.add_argument("--month", required=True)
    b = sub.add_parser("budget"); b.add_argument("--limit", action="append", default=[])
    return p

def main(argv=None):
    try:
        args = build_parser().parse_args(argv)
    except SystemExit as e:
        return 2
    try:
        entries = ledger.load(args.ledger)
    except (OSError, ledger.LedgerError) as e:
        print(f"tally: {e}", file=sys.stderr); return 2
    if args.cmd == "report" and args.category:
        c = args.category.lower()
        if c not in ledger.categories(entries): return 2
        lines = report.report([x for x in entries if x.category == c])
    elif args.cmd == "export":
        w = _csv.writer(sys.stdout, lineterminator="\n"); w.writerow(["date","category","amount","memo"])
        for x in entries: w.writerow([x.day.isoformat(), x.category, f"{x.amount:.2f}", x.memo])
        return 0
    elif args.cmd == "summary":
        if not _re.fullmatch(r"\d{4}-\d{2}", args.month): return 2
        lines = report.report([x for x in entries if x.day.isoformat().startswith(args.month)])
    elif args.cmd == "budget":
        limits = {}
        for l in args.limit:
            k, sep, v = l.partition("=")
            if not sep: return 2
            try: limits[k.lower()] = _money.parse(v)
            except _money.MoneyError: return 2
        tot = report.by_category(entries); over = False
        for k, lim in limits.items():
            if tot.get(k, _D(0)) > lim: print(f"OVER {k} {tot[k]} > {lim}"); over = True
        return 1 if over else 0
    elif args.cmd == "report": lines = report.report(entries)
    elif args.cmd == "balance": lines = report.balance(entries, args.start, args.end)
    else: lines = ledger.categories(entries)
    for line in lines: print(line)
    return 0
