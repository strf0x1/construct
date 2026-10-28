import unittest

from pkg.orders.users_orders import describe_orders_user


class T(unittest.TestCase):
    def test_orders(self):
        self.assertEqual(describe_orders_user(1), 'orders:ada')
