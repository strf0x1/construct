import unittest
from decimal import Decimal

from shop.shipping import parcel_weight, shipping_cost


class ShippingTest(unittest.TestCase):
    def test_weight_counts_quantity(self):
        self.assertAlmostEqual(parcel_weight([(1.0, 3), (0.5, 2)]), 4.25)

    def test_cost(self):
        self.assertEqual(shipping_cost([(1.0, 1)]), Decimal("5.25"))
