"""scripts/kc_env.py: every script starts knowcode with nothing of the user's (HOME and the three KnowCode directories
of its own, the update check off), and never with the real home or config directory."""

import os
from pathlib import Path
import re
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent
sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(HERE))
import common  # noqa: E402
import kc_env  # noqa: E402
import replay_gate  # noqa: E402

KEYS = ("HOME", "KNOWCODE_CACHE_DIR", "KNOWCODE_CONFIG_DIR", "KNOWCODE_DATA_DIR")

# Scripts that may name a KnowCode directory without the helper, and why. Anything else that does is a script starting
# knowcode with an environment of its own making, which is how a run came to read the user's config.
ALLOWED = {
    "kc_env.py": "the helper",
    "corpus.py": "a usage example in its docstring",
    "check-public-first-run.py": "release acceptance on a fresh account or container, where the real account is the point",
    "acceptance/adoption_tune.py": "the MCP server entry in a client session whose own environment comes from the helper",
    "eval/bare_client.py": "a client session's server entry and setup; the session runs with HOME=control/home",
    "eval/session_runner.py": "merged over bare_client's session environment, which sets HOME=control/home",
}
DIR_ASSIGNMENT = re.compile(r"KNOWCODE_(CACHE|CONFIG|DATA)_DIR['\"]?\s*(=[^=]|:)|KNOWCODE_\{")


class KnowcodeEnvTests(unittest.TestCase):
    def setUp(self):
        self.base = Path(tempfile.mkdtemp(prefix="kc-env-test-"))
        self.home = kc_env.real_home()

    def assert_isolated(self, env, base):
        for key in KEYS:
            self.assertIn(key, env)
            path = Path(env[key])
            self.assertTrue(path.is_dir(), f"{key} {path} wasn't created")
            self.assertNotEqual(path.resolve(), self.home, f"{key} is the real home")
        self.assertTrue(Path(env["HOME"]).resolve().is_relative_to(Path(base).resolve()))
        config = Path(env["KNOWCODE_CONFIG_DIR"]).resolve()
        for real in kc_env.real_config_dirs():
            self.assertFalse(config == real or config.is_relative_to(real), f"config dir {config} is the real {real}")
        self.assertEqual(env["KNOWCODE_UPDATE_CHECK"], "0")

    def test_sets_home_and_the_three_directories_under_the_base(self):
        env = kc_env.knowcode_env(self.base)
        self.assert_isolated(env, self.base)
        self.assertEqual(env["HOME"], str(self.base / "home"))
        self.assertEqual(env["KNOWCODE_CONFIG_DIR"], str(self.base / "config"))

    def test_overrides_a_real_home_and_stale_directories_in_the_start_environment(self):
        start = {"PATH": "/usr/bin:/bin", "HOME": str(self.home),
                 "KNOWCODE_CONFIG_DIR": str(kc_env.real_config_dirs()[0]), "KNOWCODE_CACHE_DIR": "/elsewhere"}
        env = kc_env.knowcode_env(self.base, start)
        self.assert_isolated(env, self.base)
        self.assertEqual(env["PATH"], "/usr/bin:/bin")

    def test_refuses_the_real_home(self):
        with self.assertRaises(ValueError):
            kc_env.knowcode_env(self.base, HOME=str(self.home))

    def test_refuses_the_real_config_directory_and_anything_inside_it(self):
        for real in kc_env.real_config_dirs():
            for config in (real, real / "sub"):
                with self.subTest(config=config), mock.patch.object(Path, "mkdir") as mkdir:
                    with self.assertRaises(ValueError):
                        kc_env.knowcode_env(self.base, config=config)
                    with self.assertRaises(ValueError):
                        kc_env.knowcode_env(self.base, KNOWCODE_CONFIG_DIR=str(config))
                    mkdir.assert_not_called()

    def test_replay_gate_build_env_is_the_helper_on_the_bare_git_environment(self):
        env = replay_gate.build_env(self.base)
        self.assert_isolated(env, self.base)
        self.assertEqual(env["PATH"], replay_gate.GIT_ENV["PATH"])
        self.assertEqual(env["GIT_CONFIG_GLOBAL"], "/dev/null")

    def test_common_kc_runs_knowcode_isolated_and_keeps_a_callers_config(self):
        cache = self.base / "cache"
        seen = {}

        def run(argv, env, **kwargs):
            seen.update(env)
            return mock.Mock(returncode=0, stdout="", stderr="")

        with mock.patch.object(common.subprocess, "run", run), mock.patch.dict(os.environ, clear=False) as environ:
            for key in ("KNOWCODE_CONFIG_DIR", "KNOWCODE_DATA_DIR"):
                environ.pop(key, None)
            common.kc("knowcode", ["--version"], cache)
            self.assert_isolated(seen, self.base)
            self.assertEqual(seen["KNOWCODE_CACHE_DIR"], str(cache))
            chosen = self.base / "query-set-config"
            environ["KNOWCODE_CONFIG_DIR"] = str(chosen)
            common.kc("knowcode", ["--version"], cache)
            self.assertEqual(seen["KNOWCODE_CONFIG_DIR"], str(chosen))
            environ["KNOWCODE_CONFIG_DIR"] = str(kc_env.real_config_dirs()[0])
            with mock.patch.object(Path, "mkdir"), self.assertRaises(ValueError):
                common.kc("knowcode", ["--version"], cache)

    def test_no_script_builds_a_knowcode_environment_of_its_own(self):
        offenders = []
        for path in sorted(SCRIPTS.rglob("*.py")):
            rel = path.relative_to(SCRIPTS).as_posix()
            if path.name.startswith("test_") or rel in ALLOWED:
                continue
            for number, line in enumerate(path.read_text().splitlines(), 1):
                if DIR_ASSIGNMENT.search(line):
                    offenders.append(f"{rel}:{number}: {line.strip()}")
        self.assertEqual(offenders, [], "build the environment with scripts/kc_env.py's knowcode_env")


if __name__ == "__main__":
    unittest.main()
