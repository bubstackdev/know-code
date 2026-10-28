"""Compile Rust and C source-location fixtures through the release wrapper without network access."""
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
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
                           env=env, check=True, capture_output=True, timeout=300)
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


C_FAKE_DIST = '''#!/usr/bin/env python3
import json, os, pathlib, shlex, subprocess, sys
assert sys.argv[1] == "build"
flags = os.environ["CFLAGS"]
assert flags == os.environ["CXXFLAGS"]
escaped = os.environ.get("CC_SHELL_ESCAPED_FLAGS")
pathlib.Path(os.environ["FIXTURE_FLAGS"]).write_text(json.dumps([flags, escaped]))
# Split CFLAGS the way the cc crate does.
words = shlex.split(flags) if escaped not in (None, "", "0", "false", "no") else flags.split()
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
objects = []
for source in os.environ["FIXTURE_SOURCES"].split(os.pathsep):
    obj = root / (pathlib.Path(source).stem + ".o")
    subprocess.run([os.environ["FIXTURE_CC"], *words, "-g", "-c", source, "-o", str(obj)], check=True, cwd=root)
    objects.append(str(obj))
subprocess.run([os.environ["FIXTURE_CC"], "-Wl,-S", *objects, "-o", os.environ["FIXTURE_BINARY"]], check=True, cwd=root)
'''


class DistBuildCTests(unittest.TestCase):
    """C dependencies (built by the cc crate from CFLAGS) get the same prefix mapping as Rust code."""

    def compile_c_fixture(self, *, cflags=None, escaped=None):
        compiler = shutil.which("cc")
        if compiler is None:
            self.skipTest("no C compiler")
        with tempfile.TemporaryDirectory(prefix="knowcode-remap-c-") as temporary:
            base = Path(temporary).resolve()
            home = base / "home with spaces"
            root = base / "checkout with spaces"
            cache = base / "cargo cache with spaces"
            (root / "scripts").mkdir(parents=True)
            cache.mkdir()
            home.mkdir()
            shutil.copyfile(Path(__file__).with_name("build-release"), root / "scripts/build-release")
            for directory, name in ((cache, "dependency"), (root, "local"), (home, "home")):
                (directory / f"{name}.c").write_text(
                    f'#include <stdio.h>\nvoid {name}(void) {{ printf("%s\\n", __FILE__); }}\n')
            (root / "main.c").write_text(
                "#include <stdio.h>\nvoid dependency(void); void local(void); void home(void);\n"
                "int main(void) { dependency(); local(); home();\n"
                '#ifdef CALLER_FLAG\nputs("caller");\n#endif\nreturn 0; }\n')
            tool = base / "fake-dist"
            tool.write_text(C_FAKE_DIST)
            tool.chmod(0o755)
            dropped = ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CFLAGS", "CXXFLAGS", "CC_SHELL_ESCAPED_FLAGS")
            env = {k: v for k, v in os.environ.items() if k not in dropped}
            sources = [cache / "dependency.c", root / "local.c", home / "home.c", root / "main.c"]
            env.update(HOME=str(home), CARGO_HOME=str(cache), FIXTURE_CC=compiler, FIXTURE_ROOT=str(root),
                       FIXTURE_SOURCES=os.pathsep.join(map(str, sources)), FIXTURE_BINARY=str(base / "fixture"),
                       FIXTURE_FLAGS=str(base / "flags.json"))
            if cflags is not None:
                env["CFLAGS"] = env["CXXFLAGS"] = cflags
            if escaped is not None:
                env["CC_SHELL_ESCAPED_FLAGS"] = escaped
            run = subprocess.run(["bash", str(root / "scripts/build-release"), str(tool), "--artifacts=local"],
                                 env=env, capture_output=True, text=True, timeout=300)
            if run.returncode:
                return run, None
            output = subprocess.check_output([str(base / "fixture")], text=True).splitlines()
            for artifact in [*root.glob("*.o"), base / "fixture"]:
                data = artifact.read_bytes()
                for prefix in (base, home, root, cache):
                    self.assertFalse(str(prefix).encode() in data, f"{artifact.name} contains {prefix}")
            return run, (output, json.loads((base / "flags.json").read_text()))

    def test_cache_checkout_and_home_paths_are_mapped(self):
        run, (output, (flags, escaped)) = self.compile_c_fixture()
        self.assertEqual(output, ["/cargo/dependency.c", "/src/local.c", "/build-home/home.c"])
        self.assertEqual(escaped, "1")
        self.assertTrue(flags.startswith("'-ffile-prefix-map="), flags)

    def test_caller_cflags_come_first(self):
        run, (output, (flags, _)) = self.compile_c_fixture(cflags="-DCALLER_FLAG=1 -O0")
        self.assertTrue(flags.startswith("-DCALLER_FLAG=1 -O0 '-ffile-prefix-map="), flags)
        self.assertEqual(output[-1], "caller")

    def test_whitespace_prefix_needs_shell_escaped_flags(self):
        run, result = self.compile_c_fixture(escaped="0")
        self.assertNotEqual(run.returncode, 0)
        self.assertIsNone(result)
        self.assertIn("CC_SHELL_ESCAPED_FLAGS", run.stderr)


class CheckBuildPathsTests(unittest.TestCase):
    """The release check finds builder prefixes inside archives and refuses to pass with nothing checked."""

    def check(self, directory: Path, home: Path):
        env = {k: v for k, v in os.environ.items() if k not in ("CARGO_HOME", "GITHUB_WORKSPACE")}
        env["HOME"] = str(home)
        return subprocess.run([sys.executable, str(Path(__file__).with_name("check-build-paths.py")), str(directory)],
                              env=env, capture_output=True, text=True, timeout=60)

    def archive(self, directory: Path, payload: bytes):
        import io
        import tarfile
        with tarfile.open(directory / "knowcode-test.tar.gz", "w:gz") as archive:
            info = tarfile.TarInfo("knowcode-test/knowcode")
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))

    def test_clean_leaky_and_empty(self):
        with tempfile.TemporaryDirectory(prefix="knowcode-paths-") as temporary:
            base = Path(temporary).resolve()
            home = base / "builder home"
            distrib = base / "distrib"
            distrib.mkdir()
            empty = self.check(distrib, home)
            self.assertEqual(empty.returncode, 1)
            self.assertIn("no files to check", empty.stderr)
            self.archive(distrib, b"\0/cargo/registry/src/x.c\0/src/lib.rs\0")
            clean = self.check(distrib, home)
            self.assertEqual(clean.returncode, 0, clean.stderr)
            for leaked in (home / ".cargo/registry/src/x.c", Path(__file__).resolve().parent / "x.rs"):
                self.archive(distrib, b"\0assert " + str(leaked).encode() + b"\0")
                leaky = self.check(distrib, home)
                self.assertEqual(leaky.returncode, 1)
                self.assertIn("knowcode-test.tar.gz!knowcode-test/knowcode", leaky.stderr)


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

    def test_native_builds_check_for_build_paths_before_attesting(self):
        local = self.release.split("\n  build-local-artifacts:\n", 1)[1].split("\n  build-global-artifacts:\n", 1)[0]
        check = "run: python3 scripts/check-build-paths.py target/distrib\n"
        self.assertEqual(self.release.count(check), 1)
        self.assertLess(local.index("bash scripts/build-release dist "), local.index(check))
        self.assertLess(local.index(check), local.index("- name: Attest\n"))

    def job(self, name: str) -> str:
        """One job's text in release.yml, up to the next job."""
        body = self.release.split(f"\n  {name}:\n", 1)[1]
        return re.split(r"\n  [a-z][a-z0-9-]*:\n", body, maxsplit=1)[0]

    def test_contents_write_only_where_a_release_is_created(self):
        self.assertIn('permissions:\n  "contents": "read"\n', self.release)
        self.assertEqual(self.release.count('"contents": "write"'), 1)
        self.assertIn('"contents": "write"', self.job("host"))
        self.assertIn("permissions:\n  contents: read\n", self.ci)
        self.assertNotIn("contents: write", self.ci)

    def test_generator_installer_is_checksum_pinned(self):
        self.assertNotIn("cargo-dist-installer.sh | sh", self.release)
        self.assertNotIn("install_dist.run", self.release)
        script = Path(__file__).with_name("install-dist.sh").read_text()
        self.assertNotIn("| sh", script)
        self.assertRegex(script, r"0\.33\.0\) expected=[0-9a-f]{64} ;;")

    def test_tags_and_secrets_reach_shell_only_through_the_environment(self):
        self.assertNotIn("axo.dev", self.release.split("\njobs:\n", 1)[1])
        for block in self.release.split("run: |")[1:]:
            body = block.split("\n      - ", 1)[0]
            self.assertNotIn("github.ref_name", body)
            self.assertNotIn("needs.plan.outputs.tag", body)
            self.assertNotIn("secrets.", body)
        # The only secret besides GITHUB_TOKEN is the tap token, and only the Homebrew publish job uses it.
        secrets = set(re.findall(r"secrets\.([A-Za-z0-9_]+)", self.release))
        self.assertEqual(secrets, {"GITHUB_TOKEN", "HOMEBREW_TAP_TOKEN"})
        token = "${{ secrets.HOMEBREW_TAP_TOKEN }}"
        self.assertEqual(self.release.count(token), 1)
        self.assertIn(token, self.job("publish-homebrew-formula"))

    def test_homebrew_job_is_least_privilege_with_its_own_commit_identity(self):
        job = self.job("publish-homebrew-formula")
        self.assertIn('    permissions:\n      "contents": "read"\n', job)
        self.assertNotIn('"write"', job)
        self.assertIn('GITHUB_USER: "KnowCode release bot"\n', job)
        email = re.search(r'GITHUB_EMAIL: "([^"]+)"\n', job).group(1)
        self.assertTrue(email.endswith("@users.noreply.github.com"), email)
        for variable in ("GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"):
            self.assertIn(f'{variable}="${{GITHUB_USER}}"', job)
        for variable in ("GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"):
            self.assertIn(f'{variable}="${{GITHUB_EMAIL}}"', job)
        # The checkout doesn't keep a credential; the tap token reaches only the push step, through `env:`.
        self.assertIn('persist-credentials: false\n          repository: "bubstackdev/homebrew-tap"\n', job)
        steps = job.split("\n      - ")
        with_token = [step for step in steps if "HOMEBREW_TAP_TOKEN" in step]
        self.assertEqual(len(with_token), 1)
        self.assertTrue(with_token[0].startswith("name: Push formula files\n        env:\n"
                                                 "          TAP_TOKEN: ${{ secrets.HOMEBREW_TAP_TOKEN }}\n"),
                        with_token[0])
        self.assertNotIn("brew ", with_token[0])
        # Publishing to the tap waits for the GitHub Release, and the announcement waits for the tap.
        self.assertIn("    needs:\n      - plan\n      - host\n", job)
        self.assertIn("needs.publish-homebrew-formula.result == 'success'", self.job("announce"))

    def test_release_gate_checks_ci_and_the_tap_and_prs_need_no_secret(self):
        workflows = Path(__file__).resolve().parent.parent / ".github/workflows"
        ci = (workflows / "ci.yml").read_text()
        gate = (workflows / "release-gate.yml").read_text()
        self.assertIn("secrets: inherit", self.release)
        self.assertIn("uses: ./.github/workflows/release-gate.yml", self.release)
        self.assertNotIn("uses: ./.github/workflows/ci.yml", self.release)
        # The tagged commit must have a passing ci.yml run; the gate may read Actions and nothing more.
        passed = gate.split("\n  ci-passed:\n", 1)[1].split("\n\n  ", 1)[0]
        self.assertIn("actions/workflows/ci.yml/runs?head_sha=$SHA", passed)
        self.assertIn('select(.conclusion == "success")', passed)
        self.assertIn("permissions:\n      actions: read\n", passed)
        self.assertIn("permissions:\n  contents: read\n", gate)
        self.assertNotIn(": write", gate)
        tap = gate.split("\n  homebrew-tap:\n", 1)[1]
        self.assertIn("gh api repos/bubstackdev/homebrew-tap", tap)
        self.assertIn(".permissions.push", tap)
        self.assertIn(".private", tap)
        self.assertIn("HOMEBREW_TAP_TOKEN is not set", tap)
        self.assertEqual(gate.count("secrets."), 1)
        self.assertIn("secrets.HOMEBREW_TAP_TOKEN", tap)
        # No CI job reads a secret, so pull requests and pushes to main run without one.
        self.assertNotIn("secrets.", ci)
        self.assertNotIn("workflow_call", ci)

    def test_dist_publishes_the_shell_installer_and_the_homebrew_formula(self):
        settings = (Path(__file__).resolve().parent.parent / "dist-workspace.toml").read_text()
        self.assertIn('installers = ["shell", "homebrew"]\n', settings)
        self.assertIn('tap = "bubstackdev/homebrew-tap"\n', settings)
        self.assertIn('publish-jobs = ["homebrew"]\n', settings)


if __name__ == "__main__":
    unittest.main()
