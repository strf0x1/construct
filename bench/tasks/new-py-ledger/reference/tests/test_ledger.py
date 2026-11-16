import unittest


class Smoke(unittest.TestCase):
    def test_imports(self):
        import ledger  # noqa: F401
