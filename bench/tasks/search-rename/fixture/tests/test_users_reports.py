import unittest

from pkg.reports.users_reports import describe_reports_user


class T(unittest.TestCase):
    def test_reports(self):
        self.assertEqual(describe_reports_user(1), 'reports:ada')
