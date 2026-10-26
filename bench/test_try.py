"""The manual launcher builds first, isolates projects, and preserves failed builds."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/try.py"
spec = importlib.util.spec_from_file_location("try_construct", SCRIPT)
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


class LauncherTest(unittest.TestCase):
    def test_fresh_copy_config_and_launch(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            project = root / "source"
            project.mkdir()
            (project / "hello.txt").write_text("original")
            (project / ".env").write_text("private")
            (project / "target").mkdir()
            (project / "target/cache").touch()
            binary = root / "built-construct"
            binary.write_text("fresh executable")
            artifact = json.dumps({"reason": "compiler-artifact", "target": {"name": "construct"},
                                   "executable": str(binary)})
            build = subprocess.CompletedProcess([], 0, artifact)
            launches = []

            def launch(path, argv, env):
                home = Path(env["CONSTRUCT_HOME"])
                work = home.parent / "project"
                self.assertEqual((work / "hello.txt").read_text(), "original")
                self.assertFalse((work / ".env").exists())
                self.assertFalse((work / "target").exists())
                self.assertEqual(Path(path).read_text(), "fresh executable")
                self.assertEqual(argv[1:], ["--model", "override"])
                self.assertEqual(env["CONSTRUCT_SANDBOX"], "workspace")
                self.assertEqual(env["CONSTRUCT_MODEL"], launcher.MODEL)
                self.assertEqual(env["OPENROUTER_API_KEY"], "sk-or-test")
                cfg = tomllib.loads((home / "config.toml").read_text())
                self.assertEqual(cfg["api_key_secret"], "OPENROUTER_API_KEY")
                self.assertEqual(cfg["sandbox"]["mode"], "workspace")
                (work / "hello.txt").write_text("edited")
                launches.append(home)

            argv = [str(SCRIPT), "--project", str(project), "--", "--model", "override"]
            with patch.object(launcher, "ROOT", root), patch.object(launcher, "KEY_FILE", root / "missing"), \
                 patch.object(launcher.subprocess, "run", return_value=build) as cargo, \
                 patch.object(launcher.os, "execve", side_effect=launch), patch.object(launcher.os, "chdir") as chdir, \
                 patch.dict(os.environ, {"OPENROUTER_API_KEY": "sk-or-test", "CONSTRUCT_SANDBOX": "off"}), \
                 patch("sys.argv", argv):
                launcher.main()
                launcher.main()
                builds = [call for call in cargo.call_args_list if call.args[0][0] == "cargo"]
                self.assertEqual(len(builds), 2)
                self.assertIn("build", builds[0].args[0])
                self.assertEqual(cargo.call_args.args[0][:3], ["git", "init", "--quiet"])
                self.assertEqual(chdir.call_args.args[0], launches[-1].parent / "project")
            self.assertNotEqual(launches[0], launches[1])
            self.assertEqual((project / "hello.txt").read_text(), "original")

    def test_failed_build_does_not_launch_stale_binary(self):
        with patch.object(launcher.subprocess, "run", return_value=subprocess.CompletedProcess([], 17, "")), \
             patch.object(launcher.os, "execve") as launch, \
             patch.object(launcher, "KEY_FILE", Path("/nonexistent/key")), patch("sys.argv", [str(SCRIPT)]):
            self.assertEqual(launcher.main(), 17)
            launch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
