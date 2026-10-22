import unittest
from decimal import Decimal

from shop.orders import advance, order_total


class OrdersTest(unittest.TestCase):
    def test_total(self):
        self.assertEqual(order_total([("10.00", 1)], "OR"), Decimal("10.00"))

    def test_advance(self):
        self.assertEqual(advance("new"), "paid")
        self.assertEqual(advance("delivered"), "delivered")
