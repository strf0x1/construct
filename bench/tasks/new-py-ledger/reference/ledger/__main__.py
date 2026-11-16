"""The reference answer: proves the hidden tests can be passed (bench/test_greenfield.py)."""

import datetime
import re
import sys
from decimal import Decimal


def parse(path):
    txns, cur = [], None
    for n, line in enumerate(open(path).read().splitlines(), 1):
        if not line.strip() or line.startswith(";"):
            continue
        if line[0].isspace():
            parts = line.split()
            cur["posts"].append([parts[0], Decimal(parts[1]) if len(parts) > 1 else None])
        else:
            date, _, desc = line.partition(" ")
            cur = {"line": n, "date": date, "desc": desc.strip(), "posts": []}
            txns.append(cur)
    for t in txns:
        try:
            if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", t["date"]):
                raise ValueError
            datetime.date.fromisoformat(t["date"])
        except ValueError:
            sys.exit(f"error: line {t['line']}: bad date")
        total = sum(a for _, a in t["posts"] if a is not None)
        missing = [p for p in t["posts"] if p[1] is None]
        if missing:
            missing[0][1] = -total
        elif total != 0:
            sys.exit(f"error: line {t['line']}: transaction does not balance")
    return sorted(txns, key=lambda t: t["date"])


def main(argv):
    opts, args, i = {}, [], 0
    while i < len(argv):
        if argv[i] in ("--from", "--to"):
            opts[argv[i]] = argv[i + 1]
            i += 2
        else:
            args.append(argv[i])
            i += 1
    cmd, path, *rest = args
    txns = [t for t in parse(path) if opts.get("--from", "0") <= t["date"] <= opts.get("--to", "9999-99-99")]
    if cmd == "balance":
        prefix, sums = (rest[0] if rest else ""), {}
        for t in txns:
            for acct, amt in t["posts"]:
                if acct.startswith(prefix):
                    sums[acct] = sums.get(acct, Decimal(0)) + amt
        for acct in sorted(sums):
            print(f"{sums[acct]:>12.2f}  {acct}")
        print("-" * 12)
        print(f"{sum(sums.values(), Decimal(0)):>12.2f}")
    else:
        running = Decimal(0)
        for t in txns:
            for acct, amt in t["posts"]:
                if acct == rest[0]:
                    running += amt
                    print(f"{t['date']} {t['desc']:<20.20} {amt:>10.2f} {running:>10.2f}")


main(sys.argv[1:])
