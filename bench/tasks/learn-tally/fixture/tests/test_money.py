import unittest
from decimal import Decimal

from tally import money


class ParseTest(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(money.parse("12.5"), Decimal("12.50"))

    def test_commas_and_dollar(self):
        self.assertEqual(money.parse("$1,234.5"), Decimal("1234.50"))

    def test_negative(self):
        self.assertEqual(money.parse("-3"), Decimal("-3.00"))

    def test_garbage(self):
        with self.assertRaises(money.MoneyError):
            money.parse("twelve")


class FmtTest(unittest.TestCase):
    def test_groups(self):
        self.assertEqual(money.fmt(Decimal("1234567.891")), "1,234,567.89")

    def test_small_and_negative(self):
        self.assertEqual(money.fmt(Decimal("-5")), "-5.00")


class SplitTest(unittest.TestCase):
    def test_even(self):
        self.assertEqual(money.split(Decimal("9.00"), 3), [Decimal("3.00")] * 3)


if __name__ == "__main__":
    unittest.main()
