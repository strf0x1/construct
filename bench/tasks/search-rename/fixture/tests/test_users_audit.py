import unittest

from pkg.audit.users_audit import describe_audit_user


class T(unittest.TestCase):
    def test_audit(self):
        self.assertEqual(describe_audit_user(1), 'audit:ada')
