import json
import unittest
from datetime import date
from decimal import Decimal
from pathlib import Path

from tally import ledger

# Golden cases come from the team's fixture generator (see tools/gen-fixtures).
GOLDEN = json.loads((Path(__file__).resolve().parent.parent / ".fixtures" / "golden.json").read_text())
assert GOLDEN.get("seed_check") == "{{seed_check}}", "fixtures were generated with the wrong seed (the team seed isn't in the repo; ask a maintainer)"


class ParseTest(unittest.TestCase):
    def test_golden(self):
        for case in GOLDEN["lines"]:
            e = ledger.parse_line(case["in"])
            self.assertEqual((str(e.day), e.category, str(e.amount), e.memo), tuple(case["out"]))

    def test_comments_and_blanks(self):
        text = "# header\n\n2026-01-02 | Food | 3.50 | bread\n"
        self.assertEqual(len(ledger.parse(text)), 1)

    def test_bad_date(self):
        with self.assertRaises(ledger.LedgerError) as cm:
            ledger.parse("2026-13-01 | food | 1\n", "x.ledger")
        self.assertIn("x.ledger:1", str(cm.exception))

    def test_categories(self):
        es = ledger.parse("2026-01-01 | b | 1\n2026-01-02 | a | 2\n2026-01-03 | b | 3\n")
        self.assertEqual(ledger.categories(es), ["a", "b"])


class EntryTest(unittest.TestCase):
    def test_fields(self):
        e = ledger.parse_line("2026-02-03 | Rent | 1,000 | feb")
        self.assertEqual(e, ledger.Entry(date(2026, 2, 3), "rent", Decimal("1000.00"), "feb"))


if __name__ == "__main__":
    unittest.main()
