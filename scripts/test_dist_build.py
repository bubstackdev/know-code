"""Compile source-location fixtures through the release wrapper without network access."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class DistBuildTests(unittest.TestCase):
    def compile_fixture(self, *, rustflags=None, encoded=None, use_cache=False, expected_flag=False):
        with tempfile.TemporaryDirectory(prefix="knowcode-remap-") as temporary:
            base = Path(temporary).resolve()
            root = base / "checkout with spaces"
            cache = base / "cargo cache with spaces"
            (root / "scripts").mkdir(parents=True)
            cache.mkdir()
            shutil.copyfile(Path(__file__).with_name("build-release"), root / "scripts/build-release")
            source = (cache if use_cache else root) / "source.rs"
            source.write_text('''#[cfg(remap_fixture)] const FLAG: &str = "present";
#[cfg(not(remap_fixture))] const FLAG: &str = "absent";
fn main() { println!("{}|{}", file!(), FLAG); }
''')
            tool = base / "fake-dist"
            tool.write_text('''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
assert sys.argv[1] == "build"
flags = os.environ["CARGO_ENCODED_RUSTFLAGS"].split("\\x1f")
pathlib.Path(os.environ["FIXTURE_ARGS"]).write_text(json.dumps(sys.argv[1:]))
subprocess.run([os.environ["FIXTURE_RUSTC"], *flags, "--crate-name", "remap_fixture",
                os.environ["FIXTURE_SOURCE"], "-o", os.environ["FIXTURE_BINARY"]], check=True)
''')
            tool.chmod(0o755)
            env = {k: v for k, v in os.environ.items() if k not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")}
            env.update(CARGO_HOME=str(cache), FIXTURE_RUSTC=shutil.which("rustc"),
                       FIXTURE_SOURCE=str(source), FIXTURE_BINARY=str(base / "fixture"),
                       FIXTURE_ARGS=str(base / "args.json"))
            if rustflags is not None:
                env["RUSTFLAGS"] = rustflags
            if encoded is not None:
                env["CARGO_ENCODED_RUSTFLAGS"] = encoded
            args = ["--artifacts=local", "argument with spaces"]
            subprocess.run(["bash", str(root / "scripts/build-release"), str(tool), *args],
                           env=env, check=True, capture_output=True, timeout=30)
            output = subprocess.check_output([str(base / "fixture")], text=True).strip()
            self.assertEqual(output, ("/cargo" if use_cache else "/knowcode") + "/source.rs|"
                             + ("present" if expected_flag else "absent"))
            self.assertNotIn(str(base), output)
            self.assertEqual(json.loads((base / "args.json").read_text()), ["build", *args])

    def test_checkout_path_and_default_flags(self):
        self.compile_fixture()

    def test_cache_path_and_multiline_rustflags(self):
        self.compile_fixture(rustflags="-C\nopt-level=0\n--cfg remap_fixture", use_cache=True, expected_flag=True)

    def test_encoded_flags_take_precedence(self):
        self.compile_fixture(rustflags="--invalid-ignored-flag", encoded="--cfg\x1fremap_fixture", expected_flag=True)

    def test_empty_encoded_flags_also_take_precedence(self):
        self.compile_fixture(rustflags="--invalid-ignored-flag", encoded="")


class ReleaseWorkflowTests(unittest.TestCase):
    """The checked-in release workflow carries scripts/generate-release.py's hardening."""

    def setUp(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("generate_release", Path(__file__).with_name("generate-release.py"))
        self.generator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.generator)
        root = Path(__file__).resolve().parent.parent
        self.release = (root / ".github/workflows/release.yml").read_text()
        self.ci = (root / ".github/workflows/ci.yml").read_text()

    def test_hardening_is_already_applied_and_idempotent(self):
        self.assertEqual(self.generator.gate(self.release), self.release)

    def test_contents_write_only_where_a_release_is_created(self):
        self.assertIn('permissions:\n  "contents": "read"\n', self.release)
        self.assertEqual(self.release.count('"contents": "write"'), 1)
        host = self.release.split("\n  host:\n", 1)[1].split("\n  announce:\n", 1)[0]
        self.assertIn('"contents": "write"', host)
        self.assertIn("permissions:\n  contents: read\n", self.ci)
        self.assertNotIn("contents: write", self.ci)

    def test_generator_installer_is_checksum_pinned(self):
        self.assertNotIn("cargo-dist-installer.sh | sh", self.release)
        self.assertNotIn("install_dist.run", self.release)
        script = Path(__file__).with_name("install-dist.sh").read_text()
        self.assertNotIn("| sh", script)
        self.assertRegex(script, r"0\.33\.0\) expected=[0-9a-f]{64} ;;")

    def test_only_the_shell_installer_is_published_and_tags_reach_shell_through_the_environment(self):
        self.assertNotIn("homebrew", self.release.lower())
        self.assertNotIn("secrets.", self.release.replace("secrets.GITHUB_TOKEN", ""))
        self.assertNotIn("axo.dev", self.release.split("\njobs:\n", 1)[1])
        for block in self.release.split("run: |")[1:]:
            body = block.split("\n      - ", 1)[0]
            self.assertNotIn("github.ref_name", body)
            self.assertNotIn("needs.plan.outputs.tag", body)


if __name__ == "__main__":
    unittest.main()
