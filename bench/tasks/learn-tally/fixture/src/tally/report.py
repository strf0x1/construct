"""Reports over a list of entries. Each function returns lines of text; the
CLI only prints them."""

from collections import defaultdict

from . import money


def by_category(entries) -> dict:
    sums = defaultdict(list)
    for e in entries:
        sums[e.category].append(e.amount)
    return {c: money.total(v) for c, v in sorted(sums.items())}


def table(totals: dict) -> list:
    """Category totals as aligned lines, then a TOTAL line."""
    if not totals:
        return ["(no entries)"]
    width = max(len(c) for c in totals) + 2
    lines = [f"{c:<{width}}{money.fmt(v):>12}" for c, v in totals.items()]
    lines.append(f"{'TOTAL':<{width}}{money.fmt(money.total(totals.values())):>12}")
    return lines


def report(entries) -> list:
    return table(by_category(entries))


def balance(entries, start=None, end=None) -> list:
    """Spending between two dates (inclusive), as one line."""
    picked = [e for e in entries if (start is None or e.day >= start) and (end is None or e.day <= end)]
    return [f"{len(picked)} entries, {money.fmt(money.total(e.amount for e in picked))}"]
