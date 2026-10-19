#!/usr/bin/env python3
"""Exercise cold MCP context, a contended writer, and eventual freshness without manual indexing.

Usage: python3 scripts/check-first-run.py /path/to/knowcode
Uses a temporary synthetic checkout and isolated config/cache/data directories.
This checks the protocol path; public-repository and clean-machine acceptance are separate gates.
"""

from __future__ import annotations

import fcntl
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time


def check(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="knowcode-first-run-") as temp:
        base = Path(temp).resolve()
        repo = base / "app"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        source = repo / "api.ts"
        source.write_text("export function apiHandler() { return 42; }\n")
        env = dict(os.environ)
        env.update({
            "KNOWCODE_CACHE_DIR": str(base / "cache"),
            "KNOWCODE_CONFIG_DIR": str(base / "config"),
            "KNOWCODE_DATA_DIR": str(base / "data"),
            "KNOWCODE_TOOLS": "core",
            "KNOWCODE_UPDATE_CHECK": "0",
        })
        env.pop("KNOWCODE_VERIFY", None)
        with (base / "stderr.log").open("w+") as stderr:
            process = subprocess.Popen(
                [str(binary), "serve"], cwd=repo, env=env,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                text=True, bufsize=1,
            )
            messages: queue.Queue = queue.Queue()

            def read() -> None:
                try:
                    for line in process.stdout:
                        messages.put(json.loads(line))
                except Exception as exc:
                    messages.put(exc)
                finally:
                    messages.put(EOFError("MCP server closed stdout"))

            reader = threading.Thread(target=read, daemon=True)
            reader.start()
            sequence = 0

            def send(value: dict) -> None:
                process.stdin.write(json.dumps(value) + "\n")
                process.stdin.flush()

            def call(method: str, params: dict) -> dict:
                nonlocal sequence
                sequence += 1
                send({"jsonrpc": "2.0", "id": sequence, "method": method, "params": params})
                deadline = time.monotonic() + 15
                while True:
                    message = messages.get(timeout=max(0.001, deadline - time.monotonic()))
                    if isinstance(message, Exception):
                        raise message
                    if message.get("id") == sequence:
                        assert "error" not in message, message
                        return message["result"]

            def context(symbol: str) -> dict:
                result = call("tools/call", {"name": "kc_context", "arguments": {"symbol": symbol}})
                assert not result.get("isError"), result
                return json.loads(result["content"][0]["text"])

            try:
                call("initialize", {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "first-run-check", "version": "1"},
                })
                send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                cold = context("apiHandler")
                assert cold["complete"] is True, cold
                assert "apiHandler" in json.dumps(cold["items"]), cold
                # MCP compacts away a fresh entry identical to the queried repository; stale entries remain strings.
                assert not any("from its last sync" in p for p in cold.get("provenance", [])), cold

                indexes = list((base / "cache" / "repos").glob("*.sqlite"))
                assert len(indexes) == 1, indexes
                writer_lock = base / "cache" / "locks" / (indexes[0].stem + ".lock")
                with writer_lock.open("r+") as writer:
                    fcntl.flock(writer, fcntl.LOCK_EX)
                    source.write_text("export function changedApiHandler() { return 4242; }\n")
                    start = time.monotonic()
                    stale = context("changedApiHandler")
                    assert time.monotonic() - start < 5, "freshness wait exceeded its budget"
                    assert stale["complete"] is False, stale
                    assert any(i["reason"] == "stale" for i in stale["incomplete"]), stale
                    assert any("from its last sync" in p for p in stale["provenance"]), stale
                    fcntl.flock(writer, fcntl.LOCK_UN)

                deadline = time.monotonic() + 15
                while True:
                    fresh = context("changedApiHandler")
                    if fresh["complete"]:
                        assert "changedApiHandler" in json.dumps(fresh["items"]), fresh
                        assert not any("from its last sync" in p for p in fresh.get("provenance", [])), fresh
                        break
                    assert time.monotonic() < deadline, fresh
                    time.sleep(0.05)

                # No query or index command after this edit: the live MCP server must warm it while idle.
                source.write_text("export function warmedApiHandler() { return 424242; }\n")
                deadline = time.monotonic() + 20
                while True:
                    # A WAL reader may need to create coordination sidecars after the last connection closed.
                    # Open this test-owned DB without creating it, then forbid application writes.
                    connection = sqlite3.connect(indexes[0].as_uri() + "?mode=rw", uri=True, timeout=1)
                    try:
                        connection.execute("PRAGMA query_only = ON")
                        warmed = connection.execute("SELECT 1 FROM symbols WHERE name = ?", ("warmedApiHandler",)).fetchone()
                    finally:
                        connection.close()
                    if warmed:
                        break
                    assert time.monotonic() < deadline, "MCP did not refresh the edited checkout while idle"
                    time.sleep(0.1)
                warm = context("warmedApiHandler")
                assert warm["complete"] is True and "warmedApiHandler" in json.dumps(warm["items"]), warm
            except Exception:
                stderr.flush()
                stderr.seek(0)
                print(stderr.read(), file=sys.stderr)
                raise
            finally:
                process.stdin.close()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                reader.join(timeout=5)
            assert process.returncode == 0, process.returncode
    print("first-run MCP check passed: cold context, bounded stale answer, background convergence, idle warming")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    check(Path(sys.argv[1]).resolve(strict=True))
