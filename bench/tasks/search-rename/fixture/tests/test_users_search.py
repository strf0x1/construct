import unittest

from pkg.search.users_search import describe_search_user


class T(unittest.TestCase):
    def test_search(self):
        self.assertEqual(describe_search_user(1), 'search:ada')
