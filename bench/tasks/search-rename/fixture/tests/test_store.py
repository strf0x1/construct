import unittest

from pkg.users.cache import prefetch_user_record
from pkg.users.store import fetch_user_record, fetch_user_records


class T(unittest.TestCase):
    def test_store(self):
        self.assertEqual(fetch_user_record(2), {'name': 'grace'})
        self.assertEqual(len(fetch_user_records([1, 2, 99])), 2)
        self.assertEqual(prefetch_user_record(3), {'name': 'edsger'})
