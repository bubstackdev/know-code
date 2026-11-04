"""Tests for release_standin.py: the curl stand-in answers the release URLs its callers ask for, and nothing else.

python3 -m unittest discover -s scripts/acceptance -p 'test_*.py'
"""
import contextlib
import hashlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import release_standin as rs  # noqa: E402

DOWNLOAD = "https://github.com/bubstackdev/know-code/releases/download"


class StandinTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.old, self.new = root / "old", root / "new"
        for d, tag in ((self.old, "v1.0.0"), (self.new, "v1.1.0")):
            d.mkdir()
            (d / "knowcode-installer.sh").write_text(f"#!/bin/sh\necho {tag}\n")
            (d / "knowcode-aarch64-apple-darwin.tar.gz").write_bytes(tag.encode() * 3)
        self.dir = root / "shim"
        self.shim = rs.prepare({"v1.0.0": self.old, "v1.1.0": self.new}, "v1.1.0", self.dir)
        self.root = root

    def tearDown(self):
        self.tmp.cleanup()

    def curl(self, *args):
        return subprocess.run([sys.executable, str(self.shim), *args], capture_output=True, cwd=self.root)

    def test_api_names_the_latest_release_and_its_digests(self):
        out = self.root / "release.json"
        r = self.curl("--disable", "--silent", "--location", "--header", "Accept: application/vnd.github+json",
                      "--output", str(out), rs.API)
        self.assertEqual(r.returncode, 0, r.stderr)
        data = json.loads(out.read_text())
        self.assertEqual((data["tag_name"], data["draft"], data["prerelease"]), ("v1.1.0", False, False))
        installer = next(a for a in data["assets"] if a["name"] == "knowcode-installer.sh")
        body = (self.new / "knowcode-installer.sh").read_bytes()
        self.assertEqual(installer["digest"], "sha256:" + hashlib.sha256(body).hexdigest())
        self.assertEqual(installer["browser_download_url"], f"{DOWNLOAD}/v1.1.0/knowcode-installer.sh")

    def test_latest_page_redirects_to_the_latest_tag(self):
        headers, body = self.root / "h", self.root / "b"
        r = self.curl("--disable", "--dump-header", str(headers), "--output", str(body), rs.PAGES + "/latest")
        self.assertEqual(r.returncode, 0, r.stderr)
        text = headers.read_text()
        self.assertTrue(text.startswith("HTTP/2 302"), text)
        self.assertIn(f"location: {rs.PAGES}/tag/v1.1.0", text)

    def test_installer_spelling_downloads_either_release(self):
        for tag, d in (("v1.0.0", self.old), ("v1.1.0", self.new)):
            out = self.root / f"{tag}.tar.gz"
            r = self.curl("-sSfL", f"{DOWNLOAD}/{tag}/knowcode-aarch64-apple-darwin.tar.gz", "-o", str(out))
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(out.read_bytes(), (d / "knowcode-aarch64-apple-darwin.tar.gz").read_bytes())

    def test_no_output_file_writes_stdout(self):
        r = self.curl("-sSfL", f"{DOWNLOAD}/v1.0.0/knowcode-installer.sh")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, (self.old / "knowcode-installer.sh").read_bytes())

    def test_other_urls_are_refused_and_logged(self):
        for url in ("https://example.com/x", f"{DOWNLOAD}/v9.9.9/knowcode-installer.sh",
                    f"{DOWNLOAD}/v1.0.0/missing.tar.gz", "https://api.github.com/repos/someone/else/releases/latest"):
            r = self.curl("-sSfL", url, "-o", str(self.root / "x"))
            self.assertEqual(r.returncode, 22, url)
            self.assertIn(b"error: 404", r.stderr)
        rows, refused = rs.requests(self.dir)
        self.assertTrue(refused)
        self.assertEqual([r["status"] for r in rows], [404] * 4)

    def test_requests_reports_a_clean_run(self):
        self.curl("-sSfL", f"{DOWNLOAD}/v1.1.0/knowcode-installer.sh", "-o", str(self.root / "i"))
        rows, refused = rs.requests(self.dir)
        self.assertFalse(refused)
        self.assertEqual([r["url"] for r in rows], [f"{DOWNLOAD}/v1.1.0/knowcode-installer.sh"])
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(rs.main(["requests", str(self.dir)]), 0)
        self.assertIn("200 " + DOWNLOAD, out.getvalue())

    def test_prepare_refuses_an_unknown_latest(self):
        with self.assertRaises(SystemExit):
            rs.prepare({"v1.0.0": self.old}, "v2.0.0", self.root / "other")


if __name__ == "__main__":
    unittest.main()
