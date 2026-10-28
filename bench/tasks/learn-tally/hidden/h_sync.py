import json
import os
import sys
import unittest
from unittest import mock

from _common import run


def fake_urlopen():
    """A mocked urlopen, patched into urllib.request and into any tally
    module that imported the name directly."""
    resp = mock.MagicMock()
    resp.status = 200
    resp.getcode.return_value = 200
    # A real server answers with a body; an implementation may parse it.
    resp.read.return_value = b"{}"
    resp.__enter__.return_value = resp
    u = mock.MagicMock(return_value=resp)
    import tally.cli  # noqa: F401  (load every module before patching names in them)

    patches = [mock.patch("urllib.request.urlopen", u)]
    for name, mod in list(sys.modules.items()):
        if name.startswith("tally") and hasattr(mod, "urlopen"):
            patches.append(mock.patch.object(mod, "urlopen", u))
    return u, patches


class Sync(unittest.TestCase):
    def call(self, env):
        u, patches = fake_urlopen()
        with mock.patch.dict(os.environ, env, clear=True):
            for p in patches:
                p.start()
            try:
                code, _, _ = run("sync")
            finally:
                for p in patches:
                    p.stop()
        return code, u

    def test_posts_the_ledger(self):
        code, u = self.call({"TALLY_URL": "https://x.example", "TALLY_TOKEN": "t0k"})
        self.assertEqual(code, 0)
        req = u.call_args[0][0]
        self.assertEqual(req.full_url, "https://x.example/ledger")
        self.assertEqual(req.get_method(), "POST")
        self.assertEqual(req.get_header("Authorization"), "Bearer t0k")
        body = json.loads(req.data)
        self.assertEqual(len(body), 9)
        self.assertEqual(body[0], {"date": "2026-02-01", "category": "rent", "amount": "1000.00", "memo": "feb"})

    def test_missing_env_is_bad_input(self):
        code, u = self.call({"TALLY_URL": "https://x.example"})
        self.assertEqual(code, 2)
        u.assert_not_called()


if __name__ == "__main__":
    unittest.main()
