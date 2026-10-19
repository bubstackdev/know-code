#!/usr/bin/env python3
"""Exercise release checks and transactional self-update without making network requests.

Usage: python3 scripts/check-updates.py [target/debug/knowcode]
Only test-owned installations, receipts, caches, and child processes are changed.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


API = "https://api.github.com/repos/bubstackdev/knowcode/releases/latest"
DOWNLOAD = "https://github.com/bubstackdev/knowcode/releases/download/v0.2.0/knowcode-installer.sh"


def check(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="knowcode-update-check-") as temp:
        root = Path(temp).resolve()
        subprocess.run(["git", "init", "--quiet", str(root)], check=True)
        (root / "example.ts").write_text("export function example() { return 1; }\n")
        tools = root / "tools"
        tools.mkdir()
        curl = tools / "curl"
        # The production endpoint is never overridden. A local executable intercepts every request and
        # rejects any URL other than the two public release URLs this fixture explicitly permits.
        curl.write_text(f"#!{sys.executable}\n" + '''import json, os, pathlib, sys, time
args = sys.argv[1:]
assert args[0] == "--disable", args
url = args[-1]
base = pathlib.Path(os.environ["UPDATE_FIXTURE"])
with (base / "requests").open("a") as f:
    f.write(json.dumps({"url": url, "args": args, "cwd": os.getcwd()}) + "\\n")
if os.environ.get("MOCK_CURL_SLEEP"):
    time.sleep(float(os.environ["MOCK_CURL_SLEEP"]))
if url == "https://api.github.com/repos/bubstackdev/knowcode/releases/latest":
    source = base / "release.json"
elif url == "https://github.com/bubstackdev/knowcode/releases/download/v0.2.0/knowcode-installer.sh":
    source = base / "installer.sh"
else:
    raise SystemExit("unexpected URL: " + url)
pathlib.Path(args[args.index("--output") + 1]).write_bytes(source.read_bytes())
''')
        curl.chmod(0o700)
        env = dict(os.environ)
        env.update(PATH=str(tools) + os.pathsep + env["PATH"], UPDATE_FIXTURE=str(root),
                   KNOWCODE_UPDATE_CHECK="0", KNOWCODE_CONFIG_DIR=str(root / "config"),
                   KNOWCODE_CACHE_DIR=str(root / "cache"), KNOWCODE_DATA_DIR=str(root / "data"),
                   XDG_CONFIG_HOME=str(root / "xdg"))
        requests = root / "requests"
        def calls() -> list[dict]:
            return [json.loads(line) for line in requests.read_text().splitlines()] if requests.exists() else []
        def run(*args: str, executable: Path = binary, extra: dict | None = None, ok: bool = True) -> subprocess.CompletedProcess:
            result = subprocess.run([str(executable), *args], env=env | (extra or {}), cwd=root,
                                    stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
            assert (result.returncode == 0) == ok, (args, result.returncode, result.stdout, result.stderr)
            return result
        installer = root / "installer.sh"
        def release(script: str, **changes: object) -> dict:
            installer.write_text(script)
            data = {"tag_name": "v0.2.0", "draft": False, "prerelease": False, "assets": [{
                "name": "knowcode-installer.sh", "browser_download_url": DOWNLOAD,
                "digest": "sha256:" + hashlib.sha256(installer.read_bytes()).hexdigest()}]}
            data.update(changes)
            (root / "release.json").write_text(json.dumps(data))
            return data
        good_script = '''#!/bin/sh
set -eu
test "$KNOWCODE_NO_MODIFY_PATH" = 1
test "$KNOWCODE_DISABLE_UPDATE" = 1
test -z "${KNOWCODE_DOWNLOAD_URL:-}"
printf '#!/bin/sh\\nprintf "knowcode 0.2.0\\\\n"\\n' > "$KNOWCODE_INSTALL_DIR/.new"
chmod 700 "$KNOWCODE_INSTALL_DIR/.new"
mv "$KNOWCODE_INSTALL_DIR/.new" "$KNOWCODE_INSTALL_DIR/knowcode"
'''
        release(good_script)

        # Opt-out and unmanaged binaries do not initiate requests. Explicit --check remains available.
        run("status")
        assert not calls() and not (root / "cache/updates").exists()
        denied = run("self-update", ok=False)
        assert "receipt" in denied.stderr and not calls()
        checked = run("self-update", "--check")
        assert "0.2.0" in checked.stdout and [c["url"] for c in calls()] == [API]
        requests.unlink()

        # Multiple simultaneous clients reserve one daily check. Notifications stay off protocol/stdout.
        enabled = env | {"KNOWCODE_UPDATE_CHECK": "1"}
        clients = [subprocess.Popen([str(binary), "status"], cwd=root, env=enabled,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(4)]
        for child in clients:
            stdout, stderr = child.communicate(timeout=15)
            assert child.returncode == 0 and b"is available" not in stdout, (stdout, stderr)
        state_path = root / "cache/updates/state.json"
        until = time.monotonic() + 10
        while time.monotonic() < until:
            state = json.loads(state_path.read_text()) if state_path.exists() else {}
            if state.get("last_success"):
                break
            time.sleep(0.02)
        assert state.get("latest") == "v0.2.0", state
        assert [c["url"] for c in calls()] == [API], calls()
        notices = [run("status", extra={"KNOWCODE_UPDATE_CHECK": "1"}) for _ in range(2)]
        assert sum("is available" in n.stderr for n in notices) <= 1
        assert all("is available" not in n.stdout for n in notices)
        assert len(calls()) == 1
        assert all(Path(c["cwd"]).is_relative_to(root / "cache/updates") for c in calls())

        # Malformed state is retained; disabling via config also avoids creating updater metadata.
        state_path.write_text("unknown state")
        run("status", extra={"KNOWCODE_UPDATE_CHECK": "1"})
        assert state_path.read_text() == "unknown state" and len(calls()) == 1
        state_path.unlink()
        run("config", "set", "update.check", "false")
        without_override = dict(env)
        without_override.pop("KNOWCODE_UPDATE_CHECK")
        result = subprocess.run([str(binary), "status"], env=without_override, cwd=root, capture_output=True, timeout=15)
        assert result.returncode == 0 and not state_path.exists() and len(calls()) == 1

        prefix = root / "bin"
        prefix.mkdir()
        installed = prefix / "knowcode"
        original = binary.read_bytes()
        receipt = root / "xdg/knowcode/knowcode-receipt.json"
        receipt.parent.mkdir(parents=True)
        receipt_text = json.dumps({"provider": {"source": "cargo-dist"}, "source": {
            "owner": "bubstackdev", "name": "knowcode", "app_name": "knowcode"},
            "install_layout": "flat", "install_prefix": str(prefix)})
        def reset() -> None:
            shutil.copy2(binary, installed)
            receipt.write_text(receipt_text)
            requests.unlink(missing_ok=True)
        reset()
        run("self-update", executable=installed, extra={"KNOWCODE_DOWNLOAD_URL": "https://invalid.example"})
        assert run("--version", executable=installed).stdout.strip() == "knowcode 0.2.0"
        assert [c["url"] for c in calls()] == [API, DOWNLOAD]

        reset()
        release(good_script, tag_name="v0.1.0")
        # Metadata URLs must match the release tag, including when no upgrade would be needed.
        bad = run("self-update", executable=installed, ok=False)
        assert "URL" in bad.stderr and installed.read_bytes() == original
        data = release(good_script, tag_name="v0.1.0")
        data["assets"][0]["browser_download_url"] = DOWNLOAD.replace("v0.2.0", "v0.1.0")
        (root / "release.json").write_text(json.dumps(data))
        requests.unlink()
        current = run("self-update", executable=installed)
        assert "is current" in current.stdout and installed.read_bytes() == original
        assert [c["url"] for c in calls()] == [API], "current versions must not download an installer"
        data = release(good_script)
        data["assets"][0]["digest"] = "sha256:" + "0" * 64
        (root / "release.json").write_text(json.dumps(data))
        bad = run("self-update", executable=installed, ok=False)
        assert "checksum mismatch" in bad.stderr and installed.read_bytes() == original

        for script in (good_script.replace("0.2.0", "0.9.0"),
                       good_script + 'printf changed > "$XDG_CONFIG_HOME/knowcode/knowcode-receipt.json"\nexit 1\n'):
            reset()
            release(script)
            failed = run("self-update", executable=installed, ok=False)
            assert "restored" in failed.stderr and installed.read_bytes() == original
            assert receipt.read_text() == receipt_text

        # A timed-out installer cannot leave its own delayed descendant running. No unrelated jobs are touched.
        reset()
        marker = root / "descendant-finished"
        release(good_script + f'(sleep 3; touch "{marker}") &\nsleep 30\n')
        failed = run("self-update", executable=installed, extra={"KNOWCODE_UPDATE_DEADLINE": "1"}, ok=False)
        assert "deadline" in failed.stderr and "restored" in failed.stderr
        assert installed.read_bytes() == original and receipt.read_text() == receipt_text
        time.sleep(3.2)
        assert not marker.exists(), "installer descendant survived its deadline"

        reset()
        release(good_script)
        failed = run("self-update", "--check", extra={"MOCK_CURL_SLEEP": "3", "KNOWCODE_UPDATE_CHECK_DEADLINE": "1"}, ok=False)
        assert "deadline" in failed.stderr and [c["url"] for c in calls()] == [API]

        # A slow failed automatic check must not hold up the invoking command or trigger an immediate retry.
        requests.unlink()
        started = time.monotonic()
        run("status", extra={"KNOWCODE_UPDATE_CHECK": "1", "MOCK_CURL_SLEEP": "3", "KNOWCODE_UPDATE_CHECK_DEADLINE": "1"})
        assert time.monotonic() - started < 2, "query waited for its automatic metadata request"
        time.sleep(1.5)
        state = json.loads(state_path.read_text())
        assert state["last_attempt"] > 0 and state["last_success"] == 0
        run("status", extra={"KNOWCODE_UPDATE_CHECK": "1"})
        assert [c["url"] for c in calls()] == [API]
    print("update checks passed: isolated metadata, opt-out, daily coordination, receipt ownership, checksum rejection, rollback, deadlines")


if __name__ == "__main__":
    if len(sys.argv) > 2:
        raise SystemExit(__doc__)
    check(Path(sys.argv[1] if len(sys.argv) == 2 else "target/debug/knowcode").resolve())
