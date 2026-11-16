"""Scores a `ledger` in the current directory by running it, never by reading it."""

import json
import subprocess
import sys
import tempfile
from pathlib import Path

JOURNAL = """; household
2026-01-09 Salary
    assets:bank    1000
    income:salary

2026-01-05 Groceries
    expenses:food    42.50
    assets:cash     -42.50
2026-01-09 A very long description that must be cut
\tassets:cash   0.10
\tassets:cash   0.20
\tassets:bank  -0.3

2026-02-01 Rent
    expenses:rent  600
    assets:bank
"""


def run(text, *args):
    with tempfile.TemporaryDirectory() as tmp:
        f = Path(tmp) / "j.txt"
        f.write_text(text)
        cmd = [a if a != "FILE" else str(f) for a in args]
        try:
            r = subprocess.run([sys.executable, "-m", "ledger", *cmd], capture_output=True, text=True, timeout=30)
        except subprocess.TimeoutExpired:
            return 124, "", ""
        return r.returncode, r.stdout.rstrip("\n").split("\n"), r.stderr.strip()


def group(fn):
    try:
        return bool(fn())
    except Exception:
        return False


def balance():
    rc, out, _ = run(JOURNAL, "balance", "FILE")
    want = ["      399.70  assets:bank", "      -42.20  assets:cash", "       42.50  expenses:food", "      600.00  expenses:rent", "    -1000.00  income:salary", "-" * 12, "        0.00"]
    rc2, out2, _ = run(JOURNAL, "balance", "FILE", "expenses")
    return rc == 0 and out == want and rc2 == 0 and out2 == ["       42.50  expenses:food", "      600.00  expenses:rent", "-" * 12, "      642.50"]


def register_file_order():
    # Turn 1's contract for one account whose postings are already in date order.
    rc, out, _ = run(JOURNAL, "register", "FILE", "assets:cash")
    return rc == 0 and out == [
        "2026-01-05 Groceries                -42.50     -42.50",
        "2026-01-09 A very long descript       0.10     -42.40",
        "2026-01-09 A very long descript       0.20     -42.20",
    ]


def errors():
    rc, _, err = run("2026-01-05 x\n  a 1\n  b -1\n\n2026-02-30 y\n  a 1\n  b -1\n", "balance", "FILE")
    rc2, _, err2 = run("; c\n\n2026-01-05 x\n  a 1.10\n  b -1.1\n2026-01-06 y\n  a 1\n  b -2\n", "register", "FILE", "a")
    return (rc, err) == (1, "error: line 5: bad date") and (rc2, err2) == (1, "error: line 6: transaction does not balance")


def date_order():
    rc, out, _ = run(JOURNAL, "register", "FILE", "assets:bank")
    return rc == 0 and out == [
        "2026-01-09 Salary                  1000.00    1000.00",
        "2026-01-09 A very long descript      -0.30     999.70",
        "2026-02-01 Rent                    -600.00     399.70",
    ] and run("2026-03-01 b\n  a 1\n  z\n2026-01-01 a\n  a 5\n  z\n", "register", "FILE", "a")[1] == [
        "2026-01-01 a                          5.00       5.00",
        "2026-03-01 b                          1.00       6.00",
    ]


def date_range():
    rc, out, _ = run(JOURNAL, "balance", "--from", "2026-01-09", "FILE", "assets", "--to", "2026-01-31")
    rc2, out2, _ = run(JOURNAL, "register", "FILE", "assets:bank", "--to", "2026-01-09")
    rc3, out3, _ = run(JOURNAL, "balance", "FILE", "--from", "2026-02-01")
    return (
        rc == 0 and out == ["      999.70  assets:bank", "        0.30  assets:cash", "-" * 12, "     1000.00"]
        and rc2 == 0 and len(out2) == 2 and out2[-1].endswith("     999.70")
        and rc3 == 0 and out3 == ["     -600.00  assets:bank", "      600.00  expenses:rent", "-" * 12, "        0.00"]
    )


def own_tests():
    if not any(Path("tests").rglob("test*.py")):
        return False
    r = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tests"], capture_output=True, text=True, timeout=120)
    return r.returncode == 0 and "Ran 0 tests" not in r.stderr


print(json.dumps({f.__name__: group(f) for f in (balance, register_file_order, errors, date_order, date_range, own_tests)}))
