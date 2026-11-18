"""Credential isolation and accounting for real benchmark endpoints, without a model."""

import tempfile
import unittest
from pathlib import Path

import provider


class Credentials(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.legacy = Path(self.tmp.name) / "openrouter.key"
        self.legacy.write_text("sk-or-test")
        self.legacy.chmod(0o600)
        self.jev = Path(self.tmp.name) / "jev.key"

    def auth(self, url, env=None):
        return provider.auth(url, env or {}, self.legacy, self.jev)

    def test_orca_uses_its_own_keyring_secret_despite_a_legacy_key(self):
        args, env = self.auth("https://api.orcarouter.ai/v1", {"OPENROUTER_API_KEY": "sk-or-other"})
        self.assertEqual(args, ["--key-secret", "ORCAROUTER_API_KEY"])
        self.assertEqual(env, {})

    def test_openrouter_keeps_its_existing_file_and_loopback_gets_no_real_key(self):
        args, env = self.auth("https://openrouter.ai/api/v1")
        self.assertEqual(args, ["--key-env", provider.TOKEN_ENV])
        self.assertEqual(env, {provider.TOKEN_ENV: "sk-or-test"})
        args, env = self.auth("http://127.0.0.1:8000/v1")
        self.assertEqual(env, {provider.TOKEN_ENV: "local"})

    def test_unknown_hosts_never_inherit_the_openrouter_key(self):
        for url in ["https://other.example/v1", "https://openrouter.ai.other.example/v1"]:
            with self.subTest(url=url), self.assertRaisesRegex(ValueError, "BENCH_API_KEY"):
                self.auth(url)

    def test_explicit_env_and_file_choices_do_not_fall_back(self):
        env = {"BENCH_API_KEY_ENV": "MY_MODEL_TOKEN", "MY_MODEL_TOKEN": "custom"}
        args, patch = self.auth("https://other.example/v1", env)
        self.assertEqual(patch, {provider.TOKEN_ENV: "custom"})
        with self.assertRaisesRegex(ValueError, "MY_MODEL_TOKEN"):
            self.auth("https://other.example/v1", {"BENCH_API_KEY_ENV": "MY_MODEL_TOKEN"})
        self.legacy.write_text("export ORCAROUTER_API_KEY='sk-orca-test'\n")
        _, patch = self.auth("https://api.orcarouter.ai/v1", {"BENCH_API_KEY_FILE": str(self.legacy)})
        self.assertEqual(patch, {provider.TOKEN_ENV: "sk-orca-test"})
        self.legacy.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "chmod 600"):
            self.auth("https://api.orcarouter.ai/v1", {"BENCH_API_KEY_FILE": str(self.legacy)})

    def test_conflicting_sources_and_known_wrong_keys_are_refused(self):
        with self.assertRaisesRegex(ValueError, "only one"):
            self.auth("https://api.orcarouter.ai/v1", {"BENCH_API_KEY_ENV": "X", "BENCH_API_KEY_SECRET": "Y"})
        for host, key in [("openrouter.ai/api", "sk-orca-test"), ("api.orcarouter.ai", "sk-or-test")]:
            with self.subTest(host=host), self.assertRaisesRegex(ValueError, "not sending"):
                self.auth(f"https://{host}/v1", {"BENCH_API_KEY_ENV": "X", "X": key})
        self.jev.write_text("sk-or-test")
        with self.assertRaisesRegex(ValueError, "Jev"):
            self.auth("https://openrouter.ai/api/v1")

    def test_cost_usd_wins_including_zero_and_missing_cost_is_unknown(self):
        self.assertEqual(provider.cost({"cost_usd": 0, "cost": 9}), 0)
        self.assertEqual(provider.cost({"cost_usd": "invalid", "cost": .5}), .5)
        self.assertIsNone(provider.cost({}))


if __name__ == "__main__":
    unittest.main()
