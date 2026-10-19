#!/usr/bin/env python3
"""Measure what MCP answers cost an agent: the size of each tool call's answer, in bytes and estimated tokens.

Usage: scripts/mcp_tokens.py --bin BIN --calls CALLS.json [--cache DIR] [--out DIR]

CALLS.json is a list of {"name": tool, "arguments": {...}}. Starts `BIN serve` once, makes every call in order and
prints, per call, the answer's size (bytes, and tokens estimated at 4 bytes each), then the total. With --out, each
answer is saved as <n>-<tool>.txt for reading. Run it with two builds on the same calls to compare them.
"""

import argparse
import json
import os
import subprocess
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--calls", required=True)
    ap.add_argument("--cache", help="KNOWCODE_CACHE_DIR for the server")
    ap.add_argument("--out", help="directory for the answers")
    a = ap.parse_args()

    env = dict(os.environ)
    if a.cache:
        env["KNOWCODE_CACHE_DIR"] = a.cache
    calls = json.loads(Path(a.calls).read_text())
    proc = subprocess.Popen([a.bin, "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env)
    next_id = [0]

    def send(method, params=None, notify=False):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not notify:
            next_id[0] += 1
            msg["id"] = next_id[0]
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        if notify:
            return None
        while True:
            reply = json.loads(proc.stdout.readline())
            if reply.get("id") == msg["id"]:
                return reply

    send("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "mcp_tokens", "version": "1"}})
    send("notifications/initialized", notify=True)
    out = Path(a.out) if a.out else None
    if out:
        out.mkdir(parents=True, exist_ok=True)
    total = 0
    print(f"{'#':>2}  {'tool':<14} {'bytes':>7} {'tokens':>7}  arguments")
    for i, c in enumerate(calls, 1):
        reply = send("tools/call", {"name": c["name"], "arguments": c.get("arguments", {})})
        result = reply.get("result") or {}
        text = "".join(part.get("text", "") for part in result.get("content", []))
        if "error" in reply:
            text = json.dumps(reply["error"])
        n = len(text.encode())
        total += n
        print(f"{i:>2}  {c['name']:<14} {n:>7} {n // 4:>7}  {json.dumps(c.get('arguments', {}))[:90]}")
        if out:
            (out / f"{i:02}-{c['name']}.txt").write_text(text)
    proc.stdin.close()
    proc.wait(timeout=30)
    print(f"total {total} bytes, about {total // 4} tokens over {len(calls)} calls")


if __name__ == "__main__":
    main()
