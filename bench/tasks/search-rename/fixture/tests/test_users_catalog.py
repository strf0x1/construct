import unittest

from pkg.catalog.users_catalog import describe_catalog_user


class T(unittest.TestCase):
    def test_catalog(self):
        self.assertEqual(describe_catalog_user(1), 'catalog:ada')
