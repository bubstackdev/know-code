#!/usr/bin/env python3
"""Measure what MCP answers cost an agent: the size of each tool call's answer, in bytes and tokens.

Usage: scripts/mcp_tokens.py --bin BIN --calls CALLS.json [--repo DIR] [--cache DIR] [--out DIR] [--json FILE]
                             [--count-tokens] [--compare BEFORE.json]

CALLS.json is a list of {"name": tool, "arguments": {...}}. Starts `BIN serve` once (in --repo, indexed first with
`BIN index`), makes every call in order and prints, per call, the answer's size: bytes, and tokens (estimated at 4
bytes each, or counted with the Anthropic token-counting endpoint with --count-tokens, which reads ANTHROPIC_API_KEY
and runs no model). With --out, each answer is saved as <n>-<tool>.txt for reading; with --json, the sizes are saved,
and --compare prints them beside a saved run's. Run it with two builds on the same calls to compare them.

--cache gives the server its own cache, config and data directories (under it), so the user's are never touched.
"""

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

ENDPOINT = "https://api.anthropic.com/v1/messages/count_tokens"
MODEL = "claude-opus-5-5"


def counter(key):
    """Tokens of a text as a tool result in a message, less the same message empty."""
    def request(text):
        return {"model": MODEL, "messages": [{"role": "user", "content": "Answer:\n" + text}]}

    def count(text):
        req = urllib.request.Request(ENDPOINT, data=json.dumps(request(text)).encode(), method="POST", headers={
            "x-api-key": key, "anthropic-version": "2023-06-01", "content-type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return json.load(r)["input_tokens"]
        except urllib.error.HTTPError as e:
            sys.exit(f"count_tokens: HTTP {e.code}: {e.read().decode(errors='replace')[:300]}")

    base = count("")
    return lambda text: count(text) - base


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--calls", required=True)
    ap.add_argument("--repo", help="the repo to serve from, indexed before the first call")
    ap.add_argument("--cache", help="a directory for the server's cache, config and data")
    ap.add_argument("--out", help="directory for the answers")
    ap.add_argument("--json", help="file for the sizes")
    ap.add_argument("--count-tokens", action="store_true", help="count tokens with the API instead of estimating")
    ap.add_argument("--compare", help="a --json file from another run, printed beside this one")
    a = ap.parse_args()

    env = dict(os.environ, KNOWCODE_UPDATE_CHECK="0")
    if a.cache:
        for name in ("CACHE", "CONFIG", "DATA"):
            env[f"KNOWCODE_{name}_DIR"] = str(Path(a.cache, name.lower()).resolve())
    tokens = (lambda text: len(text.encode()) // 4)
    if a.count_tokens:
        key = os.environ.get("ANTHROPIC_API_KEY")
        if not key:
            sys.exit("--count-tokens: ANTHROPIC_API_KEY is not set")
        tokens = counter(key)
    calls = json.loads(Path(a.calls).read_text())
    bin_ = str(Path(a.bin).resolve())
    cwd = a.repo
    if cwd:
        subprocess.run([bin_, "index"], cwd=cwd, env=env, check=True, capture_output=True)
    proc = subprocess.Popen([bin_, "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env, cwd=cwd)
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
    before = json.loads(Path(a.compare).read_text()) if a.compare else None
    sizes = []
    if before:
        print(f"{'#':>2}  {'tool':<18} {'bytes':>13} {'tokens':>13}  arguments")
    else:
        print(f"{'#':>2}  {'tool':<18} {'bytes':>7} {'tokens':>7}  arguments")
    for i, c in enumerate(calls, 1):
        reply = send("tools/call", {"name": c["name"], "arguments": c.get("arguments", {})})
        result = reply.get("result") or {}
        text = "".join(part.get("text", "") for part in result.get("content", []))
        if "error" in reply:
            text = json.dumps(reply["error"])
        n, t = len(text.encode()), tokens(text)
        sizes.append({"tool": c["name"], "arguments": c.get("arguments", {}), "bytes": n, "tokens": t})
        args = json.dumps(c.get("arguments", {}))[:70]
        if before:
            b = before[i - 1]
            print(f"{i:>2}  {c['name']:<18} {b['bytes']:>6}→{n:<6} {b['tokens']:>6}→{t:<6}  {args}")
        else:
            print(f"{i:>2}  {c['name']:<18} {n:>7} {t:>7}  {args}")
        if out:
            (out / f"{i:02}-{c['name']}.txt").write_text(text)
    proc.stdin.close()
    proc.wait(timeout=30)
    total_b, total_t = sum(s["bytes"] for s in sizes), sum(s["tokens"] for s in sizes)
    if before:
        print(f"total {sum(s['bytes'] for s in before)}→{total_b} bytes, {sum(s['tokens'] for s in before)}→{total_t} tokens "
              f"over {len(calls)} calls")
    else:
        print(f"total {total_b} bytes, {total_t} tokens over {len(calls)} calls")
    if a.json:
        Path(a.json).write_text(json.dumps(sizes, indent=1) + "\n")


if __name__ == "__main__":
    main()
