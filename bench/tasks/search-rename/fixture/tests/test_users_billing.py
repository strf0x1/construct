import unittest

from pkg.billing.users_billing import describe_billing_user


class T(unittest.TestCase):
    def test_billing(self):
        self.assertEqual(describe_billing_user(1), 'billing:ada')
