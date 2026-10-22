import unittest
from decimal import Decimal

from shop.money import format_money, round_cents, split


class MoneyTest(unittest.TestCase):
    def test_round_half_up(self):
        self.assertEqual(round_cents("10.005"), Decimal("10.01"))
        self.assertEqual(round_cents("10.004"), Decimal("10.00"))

    def test_split_sums(self):
        shares = split("10.00", 3)
        self.assertEqual(sum(shares), Decimal("10.00"))
        self.assertEqual(shares[0], Decimal("3.34"))

    def test_format(self):
        self.assertEqual(format_money("1234.5"), "$1,234.50")
