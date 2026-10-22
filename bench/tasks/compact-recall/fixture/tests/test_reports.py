import unittest

from shop.reports import spend_share, top_customers


class ReportsTest(unittest.TestCase):
    def test_top(self):
        totals = {"ann": 50, "bo": 300, "cy": 120, "di": 10}
        self.assertEqual(top_customers(totals, 2), [("bo", 300), ("cy", 120)])

    def test_share(self):
        self.assertAlmostEqual(spend_share({"a": 1, "b": 3}, "b"), 0.75)
