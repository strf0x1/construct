import unittest
from decimal import Decimal

from tally import money


class Split(unittest.TestCase):
    def check(self, amount, n):
        parts = money.split(Decimal(amount), n)
        self.assertEqual(len(parts), n)
        self.assertEqual(sum(parts), Decimal(amount), parts)
        self.assertLessEqual(max(parts) - min(parts), Decimal("0.01"), parts)
        self.assertEqual(parts, sorted(parts, reverse=True), parts)
        self.assertTrue(all(isinstance(p, Decimal) for p in parts))

    def test_thirds(self):
        self.check("100.00", 3)
        self.assertEqual(money.split(Decimal("100.00"), 3), [Decimal("33.34"), Decimal("33.33"), Decimal("33.33")])

    def test_more(self):
        for amount, n in [("10.00", 7), ("0.05", 3), ("1234.57", 6), ("9.00", 3)]:
            self.check(amount, n)

    def test_negative_refund(self):
        parts = money.split(Decimal("-10.00"), 3)
        self.assertEqual(sum(parts), Decimal("-10.00"))


if __name__ == "__main__":
    unittest.main()
