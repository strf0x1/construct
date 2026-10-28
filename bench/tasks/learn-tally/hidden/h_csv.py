import csv as csvmod
import io
import unittest

from _common import run


class Csv(unittest.TestCase):
    def rows(self):
        code, lines, _ = run("export", "--csv")
        self.assertEqual(code, 0)
        return list(csvmod.reader(io.StringIO("\n".join(lines))))

    def test_header_and_rows(self):
        rows = self.rows()
        self.assertEqual(rows[0], ["date", "category", "amount", "memo"])
        self.assertEqual(len(rows), 10)
        self.assertEqual(rows[1], ["2026-02-01", "rent", "1000.00", "feb"])

    def test_memo_with_comma_round_trips(self):
        self.assertEqual(self.rows()[2][3], "groceries, big shop")

    def test_negative(self):
        self.assertIn(["2026-03-18", "food", "-2.00", "refund"], self.rows())


if __name__ == "__main__":
    unittest.main()
