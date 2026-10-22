#!/usr/bin/env python3
"""Install a native preview, set up the real Claude plugin, and query a pinned public clone.

This checks installation and protocol behavior in an isolated profile. Fresh-account/container acceptance
must additionally validate the default install path and shell profile. No manual indexing, client config
edits, model calls, or HOME changes.
The report contains local paths and measurements: keep it outside the public snapshot pending review.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import signal
import subprocess
import tempfile
import threading
import time


def assess(case: dict, answer: dict, first_seconds: float, complete_seconds: float) -> dict:
    declarations = [item for item in answer["items"] if item.get("symbol", {}).get("file") == case["file"]
                    and item["symbol"].get("name") == case["symbol"].split(".")[-1]]
    checks = {
        "definition_file": len(declarations) == 1,
        "definition_line": any(item["symbol"].get("line") in
                               {case["definition_line"], case.get("declaration_start_line", case["definition_line"])}
                               for item in declarations),
        "source": any(not item.get("source_truncated", True) and
                      all(text in item.get("source", "") for text in case["source_contains"]) for item in declarations),
        "within_five_minutes": complete_seconds <= 300,
        "bounded_first_answer": first_seconds < 5,
    }
    if "cross_project_file" in case:
        # What every link of a route shares is said once on the route (`links_confidence`, `links_assumptions`).
        links = [{"confidence": route.get("links_confidence"), "assumptions": route.get("links_assumptions"), **link}
                 for item in declarations for route in item.get("http", []) if route.get("role") == "serves"
                 for link in route.get("links", []) if link.get("file") == case["cross_project_file"]
                 and link.get("symbol", "").endswith("." + case["cross_project_symbol"])
                 and link.get("line") == case["cross_project_line"]]
        checks["cross_project_http"] = bool(links)
        checks["http_confidence_disclosed"] = bool(links) and all(
            link.get("confidence") in ("exact", "unique_name", "ambiguous") and isinstance(link.get("assumptions"), list)
            for link in links)
    return checks


def run(args: argparse.Namespace) -> None:
    cases = json.loads(Path(__file__).with_name("acceptance").joinpath("public-repos.json").read_text())
    case = cases[args.case]
    repo = args.repo.resolve(strict=True)
    output = args.output.resolve()
    assert not output.is_relative_to(Path(__file__).resolve().parent.parent), "keep unreviewed measurements outside the source snapshot"
    output.parent.mkdir(parents=True, exist_ok=True)
    report = {"case": args.case, "expected": case, "machine": platform.platform(), "architecture": platform.machine(),
              "environment": args.environment, "quiet_confirmed_by_operator": args.quiet_confirmed,
              "profile_isolated": True, "default_paths_validated": False,
              "operator_machine_notes": args.machine_notes,
              "load_average": os.getloadavg(), "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "repo": str(repo), "status": "running", "answers": []}
    start = time.monotonic()
    def save() -> None:
        report["elapsed_seconds"] = time.monotonic() - start
        output.write_text(json.dumps(report, indent=2) + "\n")
    try:
        if args.environment == "clean-linux-x86_64-container":
            assert platform.system() == "Linux" and platform.machine() == "x86_64", "not Linux x86_64"
        if args.environment == "fresh-macos-account":
            assert platform.system() == "Darwin" and platform.machine() == "arm64", "not Apple-silicon macOS"
        if args.environment == "clean-linux-arm64":
            assert platform.system() == "Linux" and platform.machine() in ("aarch64", "arm64"), "not Linux arm64"
        def git(*params: str) -> str:
            return subprocess.check_output(["git", "-C", str(repo), *params], text=True).strip()
        assert git("rev-parse", "HEAD") == case["commit"], "clone must be at the acceptance manifest's exact commit"
        assert not git("status", "--porcelain"), "acceptance clone must be clean"
        report["origin"] = git("remote", "get-url", "origin")
        source = (repo / case["file"]).read_text(encoding="utf-8-sig")
        assert case["symbol"].split(".")[-1] in source.splitlines()[case["definition_line"] - 1]
        assert all(text in source for text in case["source_contains"]), "pinned source differs from reviewed expectations"
        with tempfile.TemporaryDirectory(prefix="knowcode-public-first-run-") as temp:
            base = Path(temp).resolve()
            env = dict(os.environ)
            for key in ("KNOWCODE_VERIFY", "KNOWCODE_UNMANAGED_INSTALL", "KNOWCODE_GITHUB_TOKEN"):
                env.pop(key, None)
            env.update(KNOWCODE_INSTALL_DIR=str(base / "bin"), KNOWCODE_NO_MODIFY_PATH="1", KNOWCODE_UPDATE_CHECK="0",
                       KNOWCODE_DOWNLOAD_URL=args.artifacts.resolve().as_uri(), KNOWCODE_CONFIG_DIR=str(base / "config"),
                       KNOWCODE_CACHE_DIR=str(base / "cache"), KNOWCODE_DATA_DIR=str(base / "data"),
                       XDG_CONFIG_HOME=str(base / "xdg"), CLAUDE_CONFIG_DIR=str(base / "claude"),
                       CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
            def command(argv: list[str], timeout: int = 180) -> str:
                child = subprocess.Popen(argv, env=env, cwd=repo, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                         stderr=subprocess.PIPE, text=True, start_new_session=True)
                try:
                    stdout, stderr = child.communicate(timeout=timeout)
                except subprocess.TimeoutExpired:
                    # This runner created the new session/group, so only this command's descendants are signalled.
                    os.killpg(child.pid, signal.SIGKILL)
                    stdout, stderr = child.communicate(timeout=5)
                    report.setdefault("commands", []).append({"argv": argv, "returncode": child.returncode,
                                                               "stdout": stdout, "stderr": stderr, "timed_out": True})
                    save()
                    raise
                report.setdefault("commands", []).append({"argv": argv, "returncode": child.returncode, "stdout": stdout, "stderr": stderr})
                save()
                assert child.returncode == 0, (argv, stdout, stderr)
                return stdout
            command(["sh", str(args.artifacts.resolve() / "knowcode-installer.sh")])
            installed = base / "bin/knowcode"
            report["binary_sha256"] = hashlib.sha256(installed.read_bytes()).hexdigest()
            report["version"] = command([str(installed), "--version"]).strip()
            command([str(installed), "setup", "claude"])
            plugins = json.loads(command(["claude", "plugin", "list", "--json"]))
            selected = [p for p in plugins if p.get("id") == "knowcode@knowcode-local"]
            assert len(selected) == 1 and selected[0].get("enabled"), plugins
            plugin_path = Path(selected[0]["installPath"])
            server = json.loads((plugin_path / ".mcp.json").read_text())["mcpServers"]["knowcode"]
            assert Path(server["command"]) == installed and server["args"] == ["serve"], server
            assert not list((base / "cache").rglob("*.sqlite")), "setup unexpectedly created an index"
            report["install_and_setup_seconds"] = time.monotonic() - start
            with (base / "stderr.log").open("w+") as stderr:
                process = subprocess.Popen([server["command"], *server["args"]], cwd=repo, env=env | server.get("env", {}),
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, bufsize=1)
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
                try:
                    call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                        "clientInfo": {"name": "public-first-run-acceptance", "version": "1"}})
                    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    first_query = time.monotonic()
                    deadline = first_query + 300
                    while True:
                        query_start = time.monotonic()
                        result = call("tools/call", {"name": "kc_context", "arguments": {
                            "symbol": case["symbol"], "source": "full", "budget": 4000}})
                        assert not result.get("isError"), result
                        answer = json.loads(result["content"][0]["text"])
                        report["answers"].append({"seconds": time.monotonic() - query_start, "answer": answer})
                        save()
                        # Converged once nothing is stale: an answer can stay incomplete for what the repo holds
                        # (`unsupported` constructs), which waiting doesn't change; which reasons may remain is the
                        # case's `allowed_incomplete` (none by default), checked below.
                        if not any(reason.get("reason") == "stale" for reason in answer.get("incomplete", [])):
                            break
                        assert time.monotonic() < deadline, "no complete answer within five minutes"
                        time.sleep(0.5)
                    report["first_answer_seconds"] = report["answers"][0]["seconds"]
                    report["complete_answer_seconds"] = time.monotonic() - first_query
                    report["checks"] = assess(case, answer, report["first_answer_seconds"], report["complete_answer_seconds"])
                    # What the converged answer still says it lacks: only the case's allowed reasons may remain.
                    remaining = [{"reason": i.get("reason"), "detail": i.get("detail")} for i in answer.get("incomplete", [])]
                    report["remaining_incomplete"] = remaining
                    allowed = set(case.get("allowed_incomplete", []))
                    report["checks"]["only_allowed_incomplete"] = all(i["reason"] in allowed for i in remaining)
                    save()
                    assert all(report["checks"].values()), report["checks"]
                finally:
                    process.stdin.close()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()  # Only the MCP process started by this runner.
                        process.wait()
                    reader.join(timeout=5)
                    stderr.seek(0)
                    report["mcp_stderr"] = stderr.read()
            command([str(installed), "uninstall", "--delete-config", "--delete-notes"])
            assert not git("status", "--porcelain"), "acceptance altered the clone"
            report["status"] = "passed"
    except Exception as exc:
        report["status"] = "failed"
        report["error"] = repr(exc)
        raise
    finally:
        save()
    print(f"{args.case}: {report['status']} ({args.environment}); report: {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", choices=["eshop", "eshop-controllers", "mitt"])
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, default=Path("target/distrib"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--environment", choices=["isolated-profile", "fresh-macos-account", "clean-linux-x86_64-container", "clean-linux-arm64"], default="isolated-profile")
    parser.add_argument("--quiet-confirmed", action="store_true", help="Operator confirms the machine was quiet; never inferred from an isolated profile")
    parser.add_argument("--machine-notes", default="", help="Record emulation, competing work, or other measurement limitations")
    run(parser.parse_args())
