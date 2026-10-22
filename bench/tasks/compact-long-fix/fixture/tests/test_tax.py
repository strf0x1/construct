import unittest
from decimal import Decimal

from shop.tax import rate_for, tax_for


class TaxTest(unittest.TestCase):
    def test_rate_any_case(self):
        self.assertEqual(rate_for("ca"), Decimal("0.0725"))
        self.assertEqual(rate_for("NY"), Decimal("0.04"))

    def test_unknown(self):
        with self.assertRaises(KeyError):
            rate_for("ZZ")

    def test_tax(self):
        self.assertEqual(tax_for("100", "tx"), Decimal("6.25"))
