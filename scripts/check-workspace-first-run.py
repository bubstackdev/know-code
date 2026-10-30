#!/usr/bin/env python3
"""Check registered-root discovery, background indexing and cross-repo HTTP context on isolated synthetic repos.

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
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402



def indexes(base: Path, name: str) -> list[Path]:
    """The indexes of the repository `name`: in the build's own tree (`cache/v/<key>/repos`), or at the cache's top
    level for 0.6.1 and older."""
    cache = base / "cache"
    return list(cache.glob(f"v/*/repos/{name}-*.sqlite")) + list(cache.glob(f"repos/{name}-*.sqlite"))

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
        env = kc_env.knowcode_env(base, KNOWCODE_TOOLS="core")
        env.pop("KNOWCODE_VERIFY", None)
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
                def indexed(root: Path) -> bool:
                    status = subprocess.run([str(binary), "status", "--repo", str(root), "--json"], env=env, cwd=base, check=True, capture_output=True, text=True)
                    status = json.loads(status.stdout)
                    return status["index"].get("exists", True) and status.get("coverage", {}).get("files", 0) > 0

                def wait_indexed(roots, what: str) -> None:
                    deadline = time.monotonic() + 60
                    while not all(indexed(r) for r in roots):
                        assert time.monotonic() < deadline, f"{what} were not indexed in the background"
                        time.sleep(0.25)

                # Every repo under the root is indexed by the server, without an `index` command or a query.
                wait_indexed([backend, web, unused], "the repos under the root")
                found = tool("kc_find", {"query": "OrderService"})
                # MCP always says `complete`.
                assert found["complete"] is True and "OrderService" in json.dumps(found["items"]), found
                deadline = time.monotonic() + 20
                while True:
                    context = tool("kc_context", {"symbol": "OrderService.load", "max_chars": 20000})
                    if context["complete"]:
                        assert "OrdersController.Orders" in json.dumps(context["items"]), context
                        break
                    assert time.monotonic() < deadline, context
                    time.sleep(0.1)
                assert indexes(base, "backend"), "the needed backend was not indexed automatically"
                # Repos cloned under the root later are noticed and indexed; one whose privacy file blocks it whole is not.
                late, locked = workspace / "late", workspace / "locked"
                for root, files in ((locked, {".knowcode-privacy.yml": "level: 1\n", "secret.py": "def kept_out():\n    return 2\n"}), (late, {"late.py": "def late_arrival():\n    return 1\n"})):
                    root.mkdir()
                    subprocess.run(["git", "init", "-q", str(root)], check=True)
                    for name, text in files.items():
                        (root / name).write_text(text)
                wait_indexed([late], "a repo cloned under the root while the server ran")
                assert not indexes(base, "locked"), "a repo blocked whole was indexed"
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
    print("workspace first-run check passed: background indexing of the roots and of a repo added later, blocked repo left out, HTTP backend linked")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    check(Path(sys.argv[1]).resolve(strict=True))
