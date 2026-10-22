import unittest

from shop import customers


class CustomersTest(unittest.TestCase):
    def test_find(self):
        customers.add("ann", " Ann@Example.com ")
        self.assertEqual(customers.find_by_email("ann@example.com")["name"], "ann")
