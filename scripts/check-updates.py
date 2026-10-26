#!/usr/bin/env python3
"""Exercise release checks and transactional self-update without making network requests.

Usage: python3 scripts/check-updates.py [target/debug/knowcode]
Only test-owned installations, receipts, caches, and child processes are changed.
"""
from __future__ import annotations

import hashlib
import json
import io
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time


API = "https://api.github.com/repos/bubstackdev/knowcode/releases/latest"
RELEASES = "https://github.com/bubstackdev/knowcode/releases/download"
LATEST = "https://github.com/bubstackdev/knowcode/releases/latest"
# The release archive self-update falls back to on this platform, as dist-workspace.toml names its targets.
TARGET = {("Darwin", "arm64"): "aarch64-apple-darwin", ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
          ("Linux", "aarch64"): "aarch64-unknown-linux-gnu"}.get((platform.system(), platform.machine()))


def check(binary: Path) -> None:
    # The fixture release is one minor version past the binary under test, so the check holds at every release.
    major, minor, _ = subprocess.run([str(binary), "--version"], capture_output=True, text=True,
                                     check=True).stdout.split()[-1].split(".")
    new = f"{major}.{int(minor) + 1}.0"
    download = f"{RELEASES}/v{new}/knowcode-installer.sh"
    archive_url = f"{RELEASES}/v{new}/knowcode-{TARGET}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="knowcode-update-check-") as temp:
        root = Path(temp).resolve()
        subprocess.run(["git", "init", "--quiet", str(root)], check=True)
        (root / "example.ts").write_text("export function example() { return 1; }\n")
        tools = root / "tools"
        tools.mkdir()
        curl = tools / "curl"
        # The production endpoint is never overridden. A local executable intercepts every request and
        # rejects any URL other than the public release URLs this fixture explicitly permits. `api-reset` makes the
        # API refuse as it does once a network's rate limit is spent; `page-down` makes the release page fail too.
        curl.write_text(f"#!{sys.executable}\n" + '''import json, os, pathlib, sys, time
args = sys.argv[1:]
assert args[0] == "--disable", args
url = args[-1]
base = pathlib.Path(os.environ["UPDATE_FIXTURE"])
headers = pathlib.Path(args[args.index("--dump-header") + 1]) if "--dump-header" in args else None
auth = [a[1:] for i, a in enumerate(args) if i and args[i - 1] == "--header" and a.startswith("@")]
with (base / "requests").open("a") as f:
    f.write(json.dumps({"url": url, "args": args, "cwd": os.getcwd(),
                        "auth": pathlib.Path(auth[0]).read_text() if auth else None}) + "\\n")
if os.environ.get("MOCK_CURL_SLEEP"):
    time.sleep(float(os.environ["MOCK_CURL_SLEEP"]))
def fail(status, code):
    sys.stderr.write(f"curl: ({code}) The requested URL returned error: {status}\\n")
    raise SystemExit(code)
if url == "https://api.github.com/repos/bubstackdev/knowcode/releases/latest":
    if (base / "api-reset").exists():
        headers.write_text("HTTP/2 403 \\r\\nx-ratelimit-limit: 60\\r\\nx-ratelimit-remaining: 0\\r\\n"
                           f"x-ratelimit-reset: {(base / 'api-reset').read_text()}\\r\\n\\r\\n")
        fail(403, 56)
    source = base / "release.json"
elif url == "@LATEST@":
    assert "--location" not in args and not auth, args
    if (base / "page-down").exists():
        fail(503, 22)
    headers.write_text("HTTP/2 302 \\r\\nlocation: @TAG@\\r\\ncontent-length: 0\\r\\n\\r\\n")
    raise SystemExit(0)
elif url == "@DOWNLOAD@":
    source = base / "installer.sh"
elif url == "@ARCHIVE@":
    source = base / "archive.tar.gz"
elif url == "@ARCHIVE@.sha256":
    source = base / "archive.sha256"
else:
    raise SystemExit("unexpected URL: " + url)
assert not auth or url.startswith("https://api.github.com/"), "the token goes only to the API"
pathlib.Path(args[args.index("--output") + 1]).write_bytes(source.read_bytes())
'''.replace("@DOWNLOAD@", download).replace("@ARCHIVE@", archive_url).replace("@LATEST@", LATEST)
            .replace("@TAG@", f"https://github.com/bubstackdev/knowcode/releases/tag/v{new}"))
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
            data = {"tag_name": f"v{new}", "draft": False, "prerelease": False, "assets": [{
                "name": "knowcode-installer.sh", "browser_download_url": download,
                "digest": "sha256:" + hashlib.sha256(installer.read_bytes()).hexdigest()}]}
            data.update(changes)
            (root / "release.json").write_text(json.dumps(data))
            return data
        good_script = '''#!/bin/sh
set -eu
test "$KNOWCODE_NO_MODIFY_PATH" = 1
test "$KNOWCODE_DISABLE_UPDATE" = 1
test -z "${KNOWCODE_DOWNLOAD_URL:-}"
printf '#!/bin/sh\\necho "${1:-}" >> "$UPDATE_FIXTURE/invoked"\\nprintf "knowcode @NEW@\\\\n"\\n' > "$KNOWCODE_INSTALL_DIR/.new"
chmod 700 "$KNOWCODE_INSTALL_DIR/.new"
mv "$KNOWCODE_INSTALL_DIR/.new" "$KNOWCODE_INSTALL_DIR/knowcode"
'''.replace("@NEW@", new)
        release(good_script)

        # Opt-out and unmanaged binaries do not initiate requests. Explicit --check remains available.
        run("status")
        assert not calls() and not (root / "cache/updates").exists()
        denied = run("self-update", ok=False)
        assert "receipt" in denied.stderr and not calls()
        checked = run("self-update", "--check")
        assert new in checked.stdout and [c["url"] for c in calls()] == [API]
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
        assert state.get("latest") == f"v{new}", state
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
            # A new file renamed into place, as self-update installs one: rewriting the installed file where it is
            # makes macOS kill the next run of it (SIGKILL, no output), the kernel holding the signature of the
            # binary that file held before. Under load that came up in about half the runs.
            fresh = prefix / ".knowcode.reset"
            shutil.copy2(binary, fresh)
            os.replace(fresh, installed)
            receipt.write_text(receipt_text)
            requests.unlink(missing_ok=True)
        reset()
        run("self-update", executable=installed, extra={"KNOWCODE_DOWNLOAD_URL": "https://invalid.example"})
        # The installed binary, which carries the new plugin bundle, refreshes it after the version check.
        assert (root / "invoked").read_text().split() == ["--version", "_refresh-plugin"]
        assert run("--version", executable=installed).stdout.strip() == f"knowcode {new}"
        assert [c["url"] for c in calls()] == [API, download]

        reset()
        release(good_script, tag_name="v0.1.0")
        # Metadata URLs must match the release tag, including when no upgrade would be needed.
        bad = run("self-update", executable=installed, ok=False)
        assert "URL" in bad.stderr and installed.read_bytes() == original
        data = release(good_script, tag_name="v0.1.0")
        data["assets"][0]["browser_download_url"] = download.replace(f"v{new}", "v0.1.0")
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

        for script in (good_script.replace(f"knowcode {new}", "knowcode 0.99.0"),
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

        # A binary in a Homebrew keg: self-update refuses before any request; --check and the notice name
        # `brew upgrade`, and only for that install.
        brew = "brew upgrade bubstackdev/tap/knowcode"
        keg = root / "Cellar/knowcode/0.1.0"
        (keg / "bin").mkdir(parents=True)
        brewed = keg / "bin/knowcode"
        shutil.copy2(binary, brewed)
        (keg / "INSTALL_RECEIPT.json").write_text("{}")
        requests.unlink()
        refused = run("self-update", executable=brewed, ok=False)
        assert f"Homebrew manages this install; run `{brew}`" in refused.stderr, refused.stderr
        assert not calls()
        checked = run("self-update", "--check", executable=brewed)
        assert f"run `{brew}`." in checked.stdout and "self-update" not in checked.stdout, checked.stdout
        clock = int(time.time())
        known = json.dumps({"format": 1, "last_attempt": clock, "last_success": clock, "latest": f"v{new}"})
        state_path.write_text(known)
        notice = run("status", executable=brewed, extra={"KNOWCODE_UPDATE_CHECK": "1"})
        assert f"v{new} is available; run `{brew}`." in notice.stderr, notice.stderr
        state_path.write_text(known)
        notice = run("status", extra={"KNOWCODE_UPDATE_CHECK": "1"})
        assert f"v{new} is available; run `knowcode self-update`." in notice.stderr, notice.stderr

        # A token, when set, goes to the API in a private header file, never in curl's arguments.
        state_path.unlink(missing_ok=True)
        requests.unlink()
        token = "fixture-token-not-a-secret"
        checked = run("self-update", "--check", extra={"KNOWCODE_GITHUB_TOKEN": token})
        assert new in checked.stdout and [c["url"] for c in calls()] == [API], calls()
        assert calls()[0]["auth"] == f"Authorization: Bearer {token}\n" and token not in json.dumps(calls()[0]["args"])
        assert not list((root / "cache/updates").glob(".knowcode-update-*")), "scratch with the token header was kept"

        # GitHub's API refuses once a network's unauthenticated rate limit is spent. The release page names the
        # latest tag, the archive is checked against the checksum published beside it, and the API is not asked
        # again until the limit resets.
        if TARGET is None:
            print(f"skipping the release archive fallback: no release archive for {platform.system()} {platform.machine()}")
        else:
            reset_at = int(time.time()) + 1800
            (root / "api-reset").write_text(str(reset_at))
            binary_script = f'#!/bin/sh\necho "${{1:-}}" >> "$UPDATE_FIXTURE/invoked"\nprintf "knowcode {new}\\n"\n'
            def archive(script: str, digest: str | None = None) -> None:
                buffer = io.BytesIO()
                with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
                    for name, text, mode in ((f"knowcode-{TARGET}/knowcode", script, 0o755), (f"knowcode-{TARGET}/README.md", "x", 0o644)):
                        info = tarfile.TarInfo(name)
                        info.size, info.mode = len(text.encode()), mode
                        tar.addfile(info, io.BytesIO(text.encode()))
                (root / "archive.tar.gz").write_bytes(buffer.getvalue())
                digest = digest or hashlib.sha256(buffer.getvalue()).hexdigest()
                (root / "archive.sha256").write_text(f"{digest} *knowcode-{TARGET}.tar.gz\n")
            archive(binary_script)
            release(good_script)
            reset()
            state_path.unlink(missing_ok=True)
            checked = run("self-update", "--check", executable=installed)
            assert new in checked.stdout and [c["url"] for c in calls()] == [API, LATEST], calls()
            assert json.loads(state_path.read_text())["api_retry_at"] == reset_at
            requests.unlink()
            (root / "invoked").unlink(missing_ok=True)
            done = run("self-update", executable=installed)
            assert [c["url"] for c in calls()] == [LATEST, archive_url + ".sha256", archive_url], calls()
            # As from the installer: the version check, then the new binary's plugin refresh and the closing message.
            assert (root / "invoked").read_text().split() == ["--version", "_refresh-plugin"]
            assert f"Installed KnowCode {new}. Start a new client session to use it." in done.stdout, done.stdout
            assert run("--version", executable=installed).stdout.strip() == f"knowcode {new}"
            # The same path and an executable mode; nothing else left in the prefix. The receipt is left as it is, as
            # the installer leaves it under KNOWCODE_DISABLE_UPDATE, so later updates and uninstall still find it.
            assert installed.stat().st_mode & 0o777 == 0o755 and sorted(p.name for p in prefix.iterdir()) == ["knowcode"]
            assert receipt.read_text() == receipt_text

            # A new binary that does not report the expected version is rolled back, as after the installer.
            reset()
            archive(binary_script.replace(f"knowcode {new}", "knowcode 0.99.0"))
            failed = run("self-update", executable=installed, ok=False)
            assert "restored" in failed.stderr and installed.read_bytes() == original, failed.stderr
            assert receipt.read_text() == receipt_text and sorted(p.name for p in prefix.iterdir()) == ["knowcode"]
            archive(binary_script)

            # The fallback never installs without its checksum: a mismatch, or no line for the archive, installs nothing.
            for digest in ("0" * 64, None):
                reset()
                archive(binary_script, digest)
                if digest is None:
                    (root / "archive.sha256").write_text(f"{'0' * 64} *knowcode-other.tar.gz\n")
                bad = run("self-update", executable=installed, ok=False)
                assert ("checksum mismatch" if digest else "does not list its archive") in bad.stderr, bad.stderr
                assert installed.read_bytes() == original and receipt.read_text() == receipt_text

            # Both refused: say why, and when to try again.
            state_path.unlink(missing_ok=True)
            requests.unlink()
            (root / "page-down").write_text("")
            failed = run("self-update", "--check", ok=False)
            expected = (f"GitHub's API rate limit for this network is used up; try again after "
                        f"{time.strftime('%H:%M', time.localtime(reset_at))}, or set KNOWCODE_GITHUB_TOKEN")
            assert expected in failed.stderr and [c["url"] for c in calls()] == [API, LATEST], failed.stderr
            (root / "page-down").unlink()

            # The automatic check reads the release page too, says nothing about the refusal, and holds the API.
            state_path.unlink(missing_ok=True)
            requests.unlink()
            quiet = run("status", extra={"KNOWCODE_UPDATE_CHECK": "1"})
            until = time.monotonic() + 10
            while time.monotonic() < until:
                state = json.loads(state_path.read_text()) if state_path.exists() else {}
                if state.get("last_success"):
                    break
                time.sleep(0.02)
            assert state.get("latest") == f"v{new}" and state.get("api_retry_at") == reset_at, state
            assert [c["url"] for c in calls()] == [API, LATEST], calls()
            assert "rate limit" not in quiet.stderr and "403" not in quiet.stderr, quiet.stderr
            (root / "api-reset").unlink()
    print("update checks passed: isolated metadata, opt-out, daily coordination, receipt ownership, checksum rejection, rollback, deadlines, Homebrew installs, API rate limits")


if __name__ == "__main__":
    if len(sys.argv) > 2:
        raise SystemExit(__doc__)
    check(Path(sys.argv[1] if len(sys.argv) == 2 else "target/debug/knowcode").resolve())
