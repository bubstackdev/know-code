#!/usr/bin/env python3
"""Check registered-root discovery and lazy cross-repo HTTP context on isolated synthetic repos.

Usage: python3 scripts/check-workspace-first-run.py /path/to/knowcode
No manual indexing or configuration edits. Public-repository acceptance is a separate gate.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time


def check(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="knowcode-workspace-first-run-") as temp:
        base = Path(temp).resolve()
        workspace = base / "workspace"
        backend, web, unused = (workspace / name for name in ("backend", "web", "unused"))
        fixtures = {
            backend: {"OrdersController.cs": "public class OrdersController : Controller {\n [HttpPost(\"Orders\")]\n public Task Orders() => null;\n}\n"},
            web: {
                "Api.ts": "export class Api { static readonly orders = '/Orders'; }\n",
                "order.service.ts": "export class OrderService {\n constructor(private http: HttpClient) {}\n load(): void { this.http.post(Api.orders, {}).subscribe(); }\n}\n",
            },
            unused: {"other.py": "def unrelated_function():\n    return 42\n"},
        }
        for root, files in fixtures.items():
            root.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            for name, text in files.items():
                (root / name).write_text(text)
        env = dict(os.environ, KNOWCODE_CACHE_DIR=str(base / "cache"), KNOWCODE_CONFIG_DIR=str(base / "config"), KNOWCODE_DATA_DIR=str(base / "data"), KNOWCODE_TOOLS="core")
        env.pop("KNOWCODE_VERIFY", None)
        env["KNOWCODE_UPDATE_CHECK"] = "0"
        subprocess.run([str(binary), "workspace", "add", str(workspace)], env=env, cwd=base, check=True, capture_output=True)
        with (base / "stderr.log").open("w+") as stderr:
            process = subprocess.Popen([str(binary), "serve"], cwd=web, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, bufsize=1)
            messages: queue.Queue = queue.Queue()

            def read() -> None:
                try:
                    for line in process.stdout:
                        messages.put(json.loads(line))
                except Exception as exc:
                    messages.put(exc)
                finally:
                    messages.put(EOFError("MCP stdout closed"))

            reader = threading.Thread(target=read, daemon=True)
            reader.start()
            sequence = 0

            def call(method: str, params: dict) -> dict:
                nonlocal sequence
                sequence += 1
                process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": sequence, "method": method, "params": params}) + "\n")
                process.stdin.flush()
                deadline = time.monotonic() + 20
                while True:
                    response = messages.get(timeout=max(0.001, deadline - time.monotonic()))
                    if isinstance(response, Exception):
                        raise response
                    if response.get("id") == sequence:
                        assert "error" not in response, response
                        return response["result"]

            def tool(name: str, arguments: dict) -> dict:
                result = call("tools/call", {"name": name, "arguments": arguments})
                assert not result.get("isError"), result
                return json.loads(result["content"][0]["text"])

            try:
                call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "workspace-first-run-check", "version": "1"}})
                process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
                process.stdin.flush()
                deadline = time.monotonic() + 25
                while True:
                    catalog = json.loads((base / "cache/discovery.json").read_text())
                    if len(catalog["entries"]) == 3 and all(e["snapshot"] is not None for e in catalog["entries"].values()):
                        break
                    assert time.monotonic() < deadline, "idle registered-root light scans did not finish"
                    time.sleep(0.1)
                assert not list((base / "cache/repos").glob("*.sqlite")), "light scans created full indexes"
                status = subprocess.run([str(binary), "status", "--repo", str(unused), "--json"], env=env, cwd=base, check=True, capture_output=True, text=True)
                status = json.loads(status.stdout)
                assert status["index"]["exists"] is False and status["discovery"] == "light", status
                assert not list((base / "cache/repos").glob("*.sqlite")), "status created a full index"
                found = tool("kc_find", {"query": "OrderService"})
                # MCP says `complete` only when it is false.
                assert found.get("complete", True) is True and "OrderService" in json.dumps(found["items"]), found
                assert not list((base / "cache/repos").glob("backend-*.sqlite")), "a local name lookup indexed the unrelated backend"
                assert not list((base / "cache/repos").glob("unused-*.sqlite")), "a local name lookup indexed the unrelated checkout"
                snapshot = subprocess.run([str(binary), "context", "OrderService.load", "--repo", str(web), "--no-sync", "--json"], env=env, cwd=base, check=True, capture_output=True, text=True)
                snapshot = json.loads(snapshot.stdout)
                assert snapshot["complete"] is False and any(i["reason"] == "stale" for i in snapshot["incomplete"]), snapshot
                assert not list((base / "cache/repos").glob("backend-*.sqlite")), "--no-sync created the backend index"
                deadline = time.monotonic() + 20
                while True:
                    context = tool("kc_context", {"symbol": "OrderService.load", "max_chars": 20000})
                    if context.get("complete", True):
                        assert "OrdersController.Orders" in json.dumps(context["items"]), context
                        break
                    assert time.monotonic() < deadline, context
                    time.sleep(0.1)
                assert list((base / "cache/repos").glob("backend-*.sqlite")), "the needed backend was not indexed automatically"
                (backend / ".knowcode-privacy.yml").write_text("level: 1\n")
                private = tool("kc_context", {"symbol": "OrderService.load", "max_chars": 20000})
                assert private["complete"] is False and any(i["reason"] == "withheld" for i in private["incomplete"]), private
                assert "OrdersController.cs" not in json.dumps(private), private
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
                    process.kill()  # Only this check's own server child.
                    process.wait()
                reader.join(timeout=5)
            assert process.returncode == 0, process.returncode
    print("workspace first-run check passed: idle light scans, lazy local query, honest no-sync answer, automatic HTTP backend indexing")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    check(Path(sys.argv[1]).resolve(strict=True))
