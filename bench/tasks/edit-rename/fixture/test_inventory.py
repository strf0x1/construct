import unittest

from app import report
from inventory import low_stock, total_qty

ITEMS = [{"name": "bolt", "qty": 2}, {"name": "nut", "qty": 10}]


class InventoryTest(unittest.TestCase):
    def test_total(self):
        self.assertEqual(total_qty(ITEMS), 12)

    def test_low(self):
        self.assertEqual([i["name"] for i in low_stock(ITEMS)], ["bolt"])

    def test_report(self):
        self.assertEqual(report(ITEMS), "total: 12\nlow: bolt")


if __name__ == "__main__":
    unittest.main()
