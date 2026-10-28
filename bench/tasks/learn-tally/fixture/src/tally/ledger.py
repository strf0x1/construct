"""The ledger file: one entry per line, `date | category | amount | memo`.
Lines starting with '#' and blank lines are ignored. Amounts are spending
(positive) or refunds (negative)."""

from dataclasses import dataclass
from datetime import date
from decimal import Decimal
from pathlib import Path

from . import money


class LedgerError(ValueError):
    def __init__(self, path, lineno, msg):
        super().__init__(f"{path}:{lineno}: {msg}")
        self.lineno = lineno


@dataclass(frozen=True)
class Entry:
    day: date
    category: str
    amount: Decimal
    memo: str = ""


def parse_line(line: str, path="<ledger>", lineno=0) -> Entry:
    fields = [f.strip() for f in line.split("|")]
    if len(fields) not in (3, 4):
        raise LedgerError(path, lineno, f"expected 3 or 4 fields, got {len(fields)}")
    try:
        day = date.fromisoformat(fields[0])
    except ValueError:
        raise LedgerError(path, lineno, f"bad date {fields[0]!r}") from None
    category = fields[1].lower()
    if not category:
        raise LedgerError(path, lineno, "empty category")
    try:
        amount = money.parse(fields[2])
    except money.MoneyError as e:
        raise LedgerError(path, lineno, str(e)) from None
    memo = fields[3] if len(fields) == 4 else ""
    return Entry(day, category, amount, memo)


def parse(text: str, path="<ledger>") -> list:
    entries = []
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        entries.append(parse_line(line, path, lineno))
    return entries


def load(path) -> list:
    p = Path(path)
    return parse(p.read_text(), str(p))


def categories(entries) -> list:
    return sorted({e.category for e in entries})
