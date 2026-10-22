import unittest
from decimal import Decimal

from shop.pricing import bulk_discount, line_price


class PricingTest(unittest.TestCase):
    def test_tiers(self):
        self.assertEqual(bulk_discount(5), Decimal("0"))
        self.assertEqual(bulk_discount(10), Decimal("0.05"))
        self.assertEqual(bulk_discount(60), Decimal("0.10"))
        self.assertEqual(bulk_discount(150), Decimal("0.15"))

    def test_line_price(self):
        self.assertEqual(line_price("2.00", 100), Decimal("170.00"))
