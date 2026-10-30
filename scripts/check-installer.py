#!/usr/bin/env python3
"""Install native dist artifacts in isolation and reject a corrupted archive.

Usage: python3 scripts/check-installer.py [target/distrib]
Run bash scripts/package-native.sh first (the local manifest must reach the global phase).
It runs with a throwaway HOME (scripts/kc_env.py) and never touches the user's shell profiles; use a fresh OS account
to validate default PATH changes.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import tempfile
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


def check(artifacts: Path) -> None:
    machine = platform.machine().lower()
    cpu = "aarch64" if machine in ("aarch64", "arm64") else "x86_64"
    suffix = "apple-darwin" if sys.platform == "darwin" else "unknown-linux-gnu"
    target = f"{cpu}-{suffix}"
    archive = artifacts / f"knowcode-{target}.tar.gz"
    installer = artifacts / "knowcode-installer.sh"
    assert archive.is_file() and installer.is_file(), "build native local and global dist artifacts first"
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    assert digest in installer.read_text(), "installer preview has no native checksum; use scripts/package-native.sh to preserve the local manifest"
    with tarfile.open(archive) as tar:
        names = {Path(member.name).name for member in tar.getmembers()}
        assert {"knowcode", "LICENSE-MIT", "LICENSE-APACHE"} <= names, names
    with tempfile.TemporaryDirectory(prefix="knowcode-installer-check-") as temp:
        base = Path(temp).resolve()
        env = kc_env.knowcode_env(base, KNOWCODE_DOWNLOAD_URL=artifacts.as_uri(), KNOWCODE_INSTALL_DIR=str(base / "bin"),
                                  KNOWCODE_NO_MODIFY_PATH="1", XDG_CONFIG_HOME=str(base / "installer-config"))
        for key in ("KNOWCODE_UNMANAGED_INSTALL", "KNOWCODE_GITHUB_TOKEN"):
            env.pop(key, None)
        def run(args: list[str]) -> subprocess.CompletedProcess:
            out = subprocess.run(args, env=env, cwd=base, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)
            assert out.returncode == 0, (args, out.stdout, out.stderr)
            return out
        run(["sh", str(installer)])
        installed = base / "bin/knowcode"
        assert installed.is_file(), "installer did not use the requested flat prefix"
        version = run([str(installed), "--version"]).stdout.strip()
        receipt = json.loads((base / "installer-config/knowcode/knowcode-receipt.json").read_text())
        assert receipt["source"]["app_name"] == "knowcode" and receipt["install_prefix"] == str(base / "bin"), receipt
        run([str(installed), "setup", "codex", "--client-dir", str(base / "codex")])
        assert json.dumps(str(installed)) in (base / "codex/config.toml").read_text()
        mirror = base / "corrupt"
        mirror.mkdir()
        (mirror / archive.name).write_bytes(archive.read_bytes() + b"corrupted-payload")
        env.update(KNOWCODE_DOWNLOAD_URL=mirror.as_uri(), KNOWCODE_INSTALL_DIR=str(base / "bad-bin"))
        bad = subprocess.run(["sh", str(installer)], env=env, cwd=base, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)
        assert bad.returncode != 0 and not (base / "bad-bin/knowcode").exists(), (bad.returncode, bad.stdout, bad.stderr)
        assert "checksum" in (bad.stdout + bad.stderr).lower(), (bad.stdout, bad.stderr)
        root = Path(__file__).resolve().parent
        subprocess.run([sys.executable, str(root / "check-first-run.py"), str(installed)], check=True)
        subprocess.run([sys.executable, str(root / "check-workspace-first-run.py"), str(installed)], check=True)
    print(f"installer check passed for {version} ({target}): isolated install, setup, checksum rejection, and first queries")


if __name__ == "__main__":
    if len(sys.argv) > 2:
        raise SystemExit(__doc__)
    check(Path(sys.argv[1] if len(sys.argv) == 2 else "target/distrib").resolve())
