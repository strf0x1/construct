import unittest

from pkg.sync.users_sync import describe_sync_user


class T(unittest.TestCase):
    def test_sync(self):
        self.assertEqual(describe_sync_user(1), 'sync:ada')
