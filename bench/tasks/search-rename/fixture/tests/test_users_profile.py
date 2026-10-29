import unittest

from pkg.profile.users_profile import describe_profile_user


class T(unittest.TestCase):
    def test_profile(self):
        self.assertEqual(describe_profile_user(1), 'profile:ada')
