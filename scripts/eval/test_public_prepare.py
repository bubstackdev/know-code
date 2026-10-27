"""Source-only preparation checks use local synthetic Git repositories, never the network or KnowCode."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "prepare-public-eval.py"
spec = importlib.util.spec_from_file_location("prepare_public_eval", SCRIPT)
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


class PublicSourceTests(unittest.TestCase):
    def git(self, repo, *args):
        return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null", "-C", str(repo), *args], text=True).strip()

    def source(self, base):
        repo = base / "source"
        repo.mkdir()
        self.git(repo, "init", "--quiet")
        (repo / "LICENSE").write_text("Synthetic fixture license notice\n")
        (repo / "src").mkdir()
        (repo / "src/api.cs").write_text("public class Api {}\n")
        (repo / "excluded").mkdir()
        (repo / "excluded/private.cs").write_text("DO_NOT_EXPORT\n")
        (repo / "hidden.cs").write_text("EXPORT_ATTRIBUTE_OMISSION\n")
        (repo / ".gitattributes").write_text("hidden.cs export-ignore\n")
        self.git(repo, "add", ".")
        self.git(repo, "-c", "user.name=Fixture Author", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "fixture")
        return repo, self.git(repo, "rev-parse", "HEAD")

    def writable(self, view):
        if view.exists():
            view.chmod(0o700)
            for path in view.rglob("*"):
                if not path.is_symlink() and path.is_dir():
                    path.chmod(0o700)

    def test_export_omissions_hashes_and_readonly_copies_are_explicit(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            repo, commit = self.source(base)
            view = base / "view"
            try:
                receipt = prepare.export(repo, commit, view, ["excluded/"])
                self.assertFalse((view / "excluded").exists())
                self.assertFalse((view / ".git").exists())
                self.assertFalse((view / "hidden.cs").exists())
                self.assertEqual(receipt["excluded_paths"], ["excluded/private.cs"])
                self.assertEqual(receipt["git_export_omissions"], ["hidden.cs"])
                self.assertNotEqual((view / "src/api.cs").stat().st_ino, (repo / "src/api.cs").stat().st_ino)
                self.assertFalse((view / "src/api.cs").stat().st_mode & 0o222)
                self.assertTrue(all(len(row["sha256"]) == 64 for row in receipt["files"]))
                with self.assertRaises(ValueError):
                    prepare.export(repo, commit, view, [])
            finally:
                self.writable(view)

    def test_escaping_links_and_unhandled_privacy_metadata_do_not_produce_ready_views(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            repo, _ = self.source(base)
            outside = base / "outside"
            outside.write_text("UNCHANGED\n")
            (repo / "link").symlink_to("../outside")
            self.git(repo, "add", "link")
            self.git(repo, "-c", "user.name=Fixture Author", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "link")
            view = base / "unsafe-view"
            with self.assertRaises(ValueError):
                prepare.export(repo, self.git(repo, "rev-parse", "HEAD"), view, [])
            self.assertEqual(outside.read_text(), "UNCHANGED\n")
            self.writable(view)
            (repo / ".knowcode-privacy.yml").write_text("level: 1\n")
            self.git(repo, "add", ".knowcode-privacy.yml")
            self.git(repo, "-c", "user.name=Fixture Author", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "privacy")
            with self.assertRaises(ValueError):
                prepare.export(repo, self.git(repo, "rev-parse", "HEAD"), base / "private-view", [])
            self.assertFalse((base / "private-view").exists())

    def test_manifest_requires_pins_public_origins_and_safe_exclusion_paths(self):
        manifest = json.loads(prepare.MANIFEST.read_text())
        self.assertEqual(len(prepare.validate(manifest)), 6)
        for field, value in (("commit", "HEAD"), ("url", "https://user:secret@github.com/owner/repo.git"), ("exclude", ["../other"])):
            bad = json.loads(json.dumps(manifest))
            bad["families"][0]["repos"][0][field] = value
            with self.assertRaises(ValueError):
                prepare.validate(bad)

    def test_nested_privacy_metadata_is_rejected_before_export(self):
        with tempfile.TemporaryDirectory() as temp:
            base=Path(temp)
            repo,_=self.source(base)
            (repo/'src/.team-privacy.yaml').write_text('tier: 1\n')
            self.git(repo,'add','src/.team-privacy.yaml')
            self.git(repo,'-c','user.name=Fixture Author','-c','user.email=fixture@example.invalid','commit','--quiet','-m','nested privacy')
            with self.assertRaisesRegex(ValueError,'privacy metadata'):
                prepare.export(repo,self.git(repo,'rev-parse','HEAD'),base/'view',[])
            self.assertFalse((base/'view').exists())


if __name__ == "__main__":
    unittest.main()
