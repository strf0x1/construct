import unittest

from pkg.accounts.users_accounts import describe_accounts_user


class T(unittest.TestCase):
    def test_accounts(self):
        self.assertEqual(describe_accounts_user(1), 'accounts:ada')
