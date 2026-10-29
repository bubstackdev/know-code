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
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


def scaled(seconds: float) -> float:
    """A time budget, stretched by how loaded the machine is (load average per core, at least 1): the server does the
    same work, but a loaded machine gives it less time. Every wait here ends on its condition; this only bounds it."""
    return seconds * max(1.0, os.getloadavg()[0] / (os.cpu_count() or 1))


def check(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="knowcode-first-run-") as temp:
        base = Path(temp).resolve()
        repo = base / "app"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        source = repo / "api.ts"
        source.write_text("export function apiHandler() { return 42; }\n")
        env = kc_env.knowcode_env(base, KNOWCODE_TOOLS="core")
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
                deadline = time.monotonic() + scaled(15)
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
                # The first answer comes from the first index. On a loaded machine that index may still be building
                # when the server's wait for it ends: the answer then says so (stale, indexing, ask again) rather
                # than answering as if nothing matched, and a later answer is whole.
                deadline = time.monotonic() + scaled(15)
                while True:
                    cold = context("apiHandler")
                    if cold["complete"]:
                        break
                    assert cold["freshness"]["status"] == "indexing", cold
                    assert all(i["reason"] == "stale" and "indexing" in i["detail"] for i in cold["incomplete"]), cold
                    assert time.monotonic() < deadline, cold
                    time.sleep(0.1)
                # A whole answer leaves out a clean, current answer's provenance; stale entries remain strings.
                assert "incomplete" not in cold, cold
                assert "apiHandler" in json.dumps(cold["items"]), cold
                assert not any("from its last sync" in p for p in cold.get("provenance", [])), cold

                indexes = list((base / "cache" / "repos").glob("*.sqlite"))
                assert len(indexes) == 1, indexes
                writer_lock = base / "cache" / "locks" / (indexes[0].stem + ".lock")
                with writer_lock.open("r+") as writer:
                    fcntl.flock(writer, fcntl.LOCK_EX)
                    source.write_text("export function changedApiHandler() { return 4242; }\n")
                    start = time.monotonic()
                    stale = context("changedApiHandler")
                    assert time.monotonic() - start < scaled(5), "freshness wait exceeded its budget"
                    assert stale["complete"] is False, stale
                    assert any(i["reason"] == "stale" for i in stale["incomplete"]), stale
                    assert any("from its last sync" in p for p in stale["provenance"]), stale
                    fcntl.flock(writer, fcntl.LOCK_UN)

                deadline = time.monotonic() + scaled(15)
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
                deadline = time.monotonic() + scaled(20)
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
                # Derived data (areas, ranking) follows the symbols in the background: the answer may say its area
                # is a moment behind until that lands, so wait for it within the same deadline.
                while True:
                    warm = context("warmedApiHandler")
                    if warm["complete"]:
                        break
                    assert all(i["reason"] == "stale" for i in warm["incomplete"]), warm
                    assert time.monotonic() < deadline, warm
                    time.sleep(0.1)
                assert "warmedApiHandler" in json.dumps(warm["items"]), warm
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
