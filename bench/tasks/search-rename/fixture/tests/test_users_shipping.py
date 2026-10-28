import unittest

from pkg.shipping.users_shipping import describe_shipping_user


class T(unittest.TestCase):
    def test_shipping(self):
        self.assertEqual(describe_shipping_user(1), 'shipping:ada')
