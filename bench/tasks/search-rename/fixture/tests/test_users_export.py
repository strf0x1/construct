import unittest

from pkg.export.users_export import describe_export_user


class T(unittest.TestCase):
    def test_export(self):
        self.assertEqual(describe_export_user(1), 'export:ada')
