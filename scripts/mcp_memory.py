#!/usr/bin/env python3
"""Measure the MCP server's resident memory under a burst of tool calls.

Usage: scripts/mcp_memory.py [--repo PATH] [--rounds N] [--limit-mb 50]

Starts `knowcode serve` in the repo, sends `rounds` rounds of calls to every tool (search four ways, two of them
over every indexed repo), and samples RSS after start, after each tool's calls, and after a short idle. Exits non-zero
if the final idle RSS is over the limit. Reads the repo only; the index lives in the user cache as usual.

What counts: the server answers some calls by running child processes (git for history, blame and diffs), and their
memory is the tool's too. Each sample is taken for the server alone and for its whole process tree (the server and
every descendant alive then, from `ps`), and a background sampler reads the tree every 20 ms so a short-lived child's
peak is caught. After the server exits, `getrusage(RUSAGE_CHILDREN).ru_maxrss` gives the largest resident size any
single process in the tree (the server, or a child it waited for) ever reached, which no sampler can miss. Sampled
peaks are a lower bound (a child living under 20 ms can slip between samples); ru_maxrss is exact but per process,
not summed over processes alive at once.
"""

import argparse
import json
import resource
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "knowcode"


def rss_mb(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out or 0) / 1024


def tree_rss_mb(pid):
    """Resident memory of `pid` and every descendant alive now, in MB."""
    out = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss="], capture_output=True, text=True).stdout
    rows = [tuple(int(x) for x in line.split()) for line in out.splitlines() if len(line.split()) == 3]
    kids = {}
    for p, pp, _ in rows:
        kids.setdefault(pp, []).append(p)
    rss = {p: r for p, _, r in rows}
    total, todo = 0, [pid]
    while todo:
        p = todo.pop()
        total += rss.get(p, 0)
        todo.extend(kids.get(p, ()))
    return total / 1024


def maxrss_children_mb():
    """The largest resident size any waited-for descendant reached, in MB (ru_maxrss is bytes on macOS, KB on
    Linux)."""
    r = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    return r / (1024 * 1024) if sys.platform == "darwin" else r / 1024


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default=str(Path.home() / "Code" / "bubstackdev" / "aether-engine"))
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--limit-mb", type=float, default=50.0)
    ap.add_argument("--pipelined", action="store_true", help="send each tool's calls at once instead of one by one")
    a = ap.parse_args()

    proc = subprocess.Popen([str(BIN), "serve"], cwd=a.repo, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    next_id = [0]

    def send_many(method, params, n):
        ids = []
        for _ in range(n):
            next_id[0] += 1
            ids.append(next_id[0])
            proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id[0], "method": method, "params": params}) + "\n")
        proc.stdin.flush()
        pending = set(ids)
        while pending:
            reply = json.loads(proc.stdout.readline())
            pending.discard(reply.get("id"))

    def call(method, params=None, notify=False):
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
            line = proc.stdout.readline()
            if not line:
                raise SystemExit("server exited")
            reply = json.loads(line)
            if reply.get("id") == next_id[0]:
                if "error" in reply:
                    raise SystemExit(f"{method}: {reply['error']}")
                return reply["result"]

    call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mem", "version": "0"}})
    call("notifications/initialized", notify=True)
    samples = [("start", rss_mb(proc.pid), tree_rss_mb(proc.pid))]
    # Peaks between the samples: the whole tree every 20 ms while the calls run.
    tree_peak = [0.0]
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            tree_peak[0] = max(tree_peak[0], tree_rss_mb(proc.pid))
            stop.wait(0.02)

    threading.Thread(target=sampler, daemon=True).start()

    names = call("tools/call", {"name": "kc_find", "arguments": {"query": "engine", "limit": 5}})
    first = json.loads(names["content"][0]["text"])["items"]
    symbol = first[0]["name"] if first else "main"
    calls = {
        "kc_find": {"query": "engine"},
        "kc_refs": {"name": "init"},
        "kc_context": {"symbol": symbol},
        "kc_trace": {"symbol": symbol, "depth": 3},
        "kc_impact": {"symbol": symbol, "depth": 4},
        "kc_hotspots": {},
        "kc_overview": {},
        "kc_status": {},
        "kc_search": {"pattern": "self", "limit": 50},
        "kc_search (regex, context)": {"pattern": r"func [a-z]+\(", "regex": True, "context": 2},
        "kc_search (workspace)": {"pattern": "error", "case": "insensitive", "workspace": True},
        "kc_search (workspace, no trigram)": {"pattern": "id", "word": True, "workspace": True, "files_only": True},
    }
    for label, args in calls.items():
        name = label.split(" ")[0]
        t = time.perf_counter()
        if a.pipelined:
            send_many("tools/call", {"name": name, "arguments": args}, a.rounds)
        else:
            for _ in range(a.rounds):
                call("tools/call", {"name": name, "arguments": args})
        ms = (time.perf_counter() - t) * 1000 / a.rounds
        samples.append((f"{label} x{a.rounds} ({ms:.0f} ms/call)", rss_mb(proc.pid), tree_rss_mb(proc.pid)))
    time.sleep(3)
    samples.append(("idle 3 s", rss_mb(proc.pid), tree_rss_mb(proc.pid)))
    stop.set()
    proc.stdin.close()
    proc.wait(timeout=10)
    largest = maxrss_children_mb()

    print(f"repo: {a.repo} ({'pipelined' if a.pipelined else 'sequential'}, {a.rounds} calls per tool)")
    print("  server     tree")
    for label, mb, tree in samples:
        print(f"  {mb:6.1f}  {tree:6.1f} MB  {label}")
    peak = max(mb for _, mb, _ in samples)
    tree = max(tree_peak[0], max(t for _, _, t in samples))
    final = samples[-1][1]
    print(f"server: peak {peak:.1f} MB, final {final:.1f} MB, limit {a.limit_mb:.0f} MB")
    print(f"process tree (server and children, sampled every 20 ms): peak {tree:.1f} MB")
    print(f"largest single process in the tree (ru_maxrss of children): {largest:.1f} MB")
    sys.exit(0 if final <= a.limit_mb else 1)


if __name__ == "__main__":
    main()
