import unittest

from pkg.notify.users_notify import describe_notify_user


class T(unittest.TestCase):
    def test_notify(self):
        self.assertEqual(describe_notify_user(1), 'notify:ada')
