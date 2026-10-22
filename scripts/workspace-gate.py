#!/usr/bin/env python3
"""The workspace gate: a public workspace of 100+ repos is indexed and kept current by `knowcode serve` alone.

Usage: scripts/workspace-gate.py [--bin PATH] [--list FILE] [--workspace DIR] [--work DIR] [--timeout SECONDS]
                                 [--clone-only]

1. Clones every repo in --list (default scripts/workspace-gate/repos.txt: `owner/repo commit` per line) shallowly at
   its pinned commit under --workspace (default ~/Library/Caches/dev.bubstack.knowcode/public-eval/0.3/workspace),
   except the last two, held back under --work to be added later. Clones are reused across runs and never written to
   otherwise.
2. Starts `knowcode serve` with that directory as its only workspace root, no `index` command, an unlimited cache
   (`cache.max_size = 0`) and caches, config and data under --work.
3. While it indexes, asks workspace-wide questions and checks that no repo is left out silently: every repo of the
   workspace without a full index all through the answer is named or counted in its "not yet indexed" entry.
4. Waits until `status` shows every repo indexed and current (nothing stale or removed since its last sync).
5. Moves the first held-back repo under the root and, asking nothing, waits until the server notices and indexes it.
   Then moves the second one in, asks at once (the answer must read it or say it isn't indexed yet), and waits for it
   too.

Prints a Markdown report and writes the details to <work>/gate.json. Exit status 1 when a check fails.

Correctness checks can run on any machine. The times it reports are wall-clock and indicative only on a loaded
machine: run it on a quiet one before quoting them. Per-phase costs come from `scripts/index-phases.py`.
"""

import argparse
import concurrent.futures
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_LIST = ROOT / "scripts" / "workspace-gate" / "repos.txt"
DEFAULT_WORKSPACE = Path.home() / "Library/Caches/dev.bubstack.knowcode/public-eval/0.3/workspace"
NOT_INDEXED = re.compile(r"^workspace: (\d+) repositor(?:y|ies) found, not yet indexed \((.*)\); nothing from")


def read_list(path):
    repos = []
    for line in path.read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            name, commit = line.split()
            repos.append((name, commit))
    return repos


def checkout_dir(base, name):
    # owner/repo -> owner--repo: unique, and one directory level under the root.
    return base / name.replace("/", "--")


def clone(name, commit, dest):
    if (dest / ".git").exists():
        head = subprocess.run(["git", "-C", str(dest), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        if head == commit:
            return "reused"
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    git = lambda *a: subprocess.run(["git", "-C", str(dest), *a], check=True, capture_output=True)
    git("init", "-q")
    git("remote", "add", "origin", f"https://github.com/{name}.git")
    git("fetch", "-q", "--depth", "1", "origin", commit)
    git("-c", "advice.detachedHead=false", "checkout", "-q", "FETCH_HEAD")
    return "cloned"


class Server:
    """`knowcode serve` over stdio."""

    def __init__(self, binary, env, cwd, log):
        self.log = open(log, "w")
        self.proc = subprocess.Popen([str(binary), "serve"], cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True, bufsize=1)
        self.messages = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.seq = 0
        self.call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "workspace-gate", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _read(self):
        for line in self.proc.stdout:
            self.messages.put(json.loads(line))
        self.messages.put(EOFError("server closed its output"))

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def call(self, method, params, timeout=600):
        self.seq += 1
        self._send({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while True:
            m = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(m, Exception):
                raise m
            if m.get("id") == self.seq:
                if "error" in m:
                    raise RuntimeError(m["error"])
                return m["result"]

    def tool(self, name, args):
        result = self.call("tools/call", {"name": name, "arguments": args})
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise RuntimeError(f"{name}: {text}")
        return json.loads(text)

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()  # only the gate's own server
            self.proc.wait()
        self.log.close()


def status(binary, env, cwd):
    """Every repo of the workspace, as `status --json` (run outside any repo) lists it: name -> state."""
    out = subprocess.run([str(binary), "status", "--json"], cwd=cwd, env=env, capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"status: {out.stderr.strip()}")
    rows = json.loads(out.stdout)
    rows = rows if isinstance(rows, list) else [rows]
    states = {}
    for r in rows:
        name = Path(r["repo"]).name
        if r.get("index", {}).get("exists") is False:
            states[name] = r.get("background") or r.get("discovery") or "unindexed"
        elif r.get("privacy", {}).get("blocked") == "repo":
            states[name] = "blocked"
        elif r.get("coverage", {}).get("files", 0) == 0 and r.get("background"):
            states[name] = "indexing"
        elif r.get("freshness", {}).get("stale_files", 0) or r.get("freshness", {}).get("removed_files", 0):
            states[name] = "stale"
        else:
            states[name] = "current"
    return states


def unaccounted(answer, waiting):
    """Of the repos `waiting` for their index all through a workspace-wide answer, those it doesn't say it left out.

    An MCP answer doesn't list the repos it read; it reads every repo with a full index, and says how many others there
    are, naming up to ten, in one "not yet indexed" entry. So every waiting repo must be named there, or counted when
    the entry names only ten."""
    named, count = set(), 0
    for i in answer.get("incomplete", []):
        m = NOT_INDEXED.match(i.get("detail", ""))
        if m:
            count = int(m.group(1))
            listed = m.group(2)
            if ", and " in listed and listed.endswith(" more"):
                listed = listed.rsplit(", and ", 1)[0]
            named |= {n.strip() for n in listed.split(",") if n.strip()}
    if answer.get("complete") is not False and waiting:
        return sorted(waiting)
    unnamed = sorted(n for n in waiting if n not in named)
    # Past the ten it names, the entry counts: enough of them to cover the rest.
    return unnamed if len(unnamed) > count - len(named) else []


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", type=Path, default=ROOT / "target" / "release" / "knowcode")
    ap.add_argument("--list", type=Path, default=DEFAULT_LIST)
    ap.add_argument("--workspace", type=Path, default=DEFAULT_WORKSPACE)
    ap.add_argument("--work", type=Path, required=True, help="caches, config, logs and the held-back clone")
    ap.add_argument("--timeout", type=float, default=4 * 3600, help="seconds to wait for every repo to be indexed")
    ap.add_argument("--poll", type=float, default=15)
    ap.add_argument("--clone-only", action="store_true")
    a = ap.parse_args()

    repos = read_list(a.list)
    if len(repos) < 3:
        sys.exit("the list needs at least three repos")
    *initial, quiet, asked = repos
    workspace, work = a.workspace.resolve(), a.work.resolve()
    workspace.mkdir(parents=True, exist_ok=True)
    held = work / "held-back"
    held.mkdir(parents=True, exist_ok=True)
    late = [(n, c, checkout_dir(workspace, n), checkout_dir(held, n)) for n, c in (quiet, asked)]
    for _, _, in_ws, kept in late:
        if in_ws.exists():
            # Added by an earlier run: held back again.
            shutil.rmtree(kept, ignore_errors=True)
            shutil.move(str(in_ws), str(kept))
    listed = {checkout_dir(workspace, n).name for n, _ in initial}
    strays = sorted(p.name for p in workspace.iterdir() if p.is_dir() and p.name not in listed)
    if strays:
        sys.exit(f"{workspace} holds repos not on the list: {', '.join(strays)}")

    t0 = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(8) as pool:
        jobs = {pool.submit(clone, n, c, checkout_dir(workspace, n)): n for n, c in initial}
        for n, c, _, kept in late:
            jobs[pool.submit(clone, n, c, kept)] = n
        failed = []
        for f in concurrent.futures.as_completed(jobs):
            try:
                f.result()
            except Exception as e:  # noqa: BLE001 - reported, then the gate stops
                failed.append(f"{jobs[f]}: {e}")
    if failed:
        sys.exit("clones failed:\n" + "\n".join(failed))
    print(f"{len(initial)} repos under {workspace}, {quiet[0]} and {asked[0]} held back ({time.monotonic() - t0:.0f} s to clone)")
    if a.clone_only:
        return

    for d in ("cache", "config", "data"):
        shutil.rmtree(work / d, ignore_errors=True)
    (work / "config").mkdir(parents=True)
    (work / "config" / "config.toml").write_text(f'[workspace]\nroots = [{json.dumps(str(workspace))}]\n\n[cache]\nmax_size = 0\n')
    env = dict(os.environ, KNOWCODE_CACHE_DIR=str(work / "cache"), KNOWCODE_CONFIG_DIR=str(work / "config"), KNOWCODE_DATA_DIR=str(work / "data"))
    env["KNOWCODE_UPDATE_CHECK"] = "0"
    env.pop("KNOWCODE_VERIFY", None)
    outside = work / "outside"
    outside.mkdir(exist_ok=True)

    expected = {checkout_dir(workspace, n).name for n, _ in initial}
    problems, timeline, checks, all_indexed, answers = [], [], 0, None, []

    def check_answers(expected_now, label):
        nonlocal checks
        for name, args in (("kc_search", {"pattern": "TODO", "workspace": True, "files_only": True, "max_chars": 100000}),
                           ("kc_locate", {"query": "configuration", "workspace": True, "max_chars": 100000})):
            before = status(a.bin, env, outside)
            try:
                answer = server.tool(name, args)
            except Exception as e:  # noqa: BLE001
                problems.append(f"{label}: {name} failed: {e}")
                continue
            after = status(a.bin, env, outside)
            checks += 1
            # Waiting all through the answer: not indexed before it and still not after (or not listed at all).
            waiting = {n for n in expected_now if before.get(n) not in ("current", "blocked") and after.get(n) not in ("current", "blocked")}
            missing = unaccounted(answer, waiting)
            said = [i["detail"] for i in answer.get("incomplete", []) if NOT_INDEXED.match(i.get("detail", ""))]
            answers.append({"at": label, "tool": name, "waiting": len(waiting), "said": said[0] if said else None})
            if missing:
                problems.append(f"{label}: {name} left out {len(missing)} repo(s) not yet indexed without saying so: {', '.join(missing[:10])}")

    started = time.monotonic()
    server = Server(a.bin, env, outside, work / "serve.log")
    try:
        # 1. Every repo indexed and current, with no `index` command.
        while True:
            states = status(a.bin, env, outside)
            counts = {}
            for s in states.values():
                counts[s] = counts.get(s, 0) + 1
            elapsed = time.monotonic() - started
            timeline.append({"s": round(elapsed), "counts": counts})
            print(f"  {elapsed:7.0f} s  " + "  ".join(f"{k} {v}" for k, v in sorted(counts.items())), flush=True)
            unknown = expected - set(states)
            if unknown:
                problems.append(f"status doesn't list {len(unknown)} repo(s): {', '.join(sorted(unknown)[:10])}")
                break
            check_answers(expected, f"at {elapsed:.0f} s")
            if all(states[n] in ("current", "blocked") for n in expected):
                all_indexed = elapsed
                break
            if elapsed > a.timeout:
                problems.append(f"not every repo indexed after {a.timeout:.0f} s: {counts}")
                all_indexed = None
                break
            time.sleep(a.poll)

        # 2. A repo added under the root is noticed and indexed by the server alone (nothing asked meanwhile), and a
        # repo added and asked about at once is read or said not indexed yet, then indexed.
        added_at = {}
        for (n, _, in_ws, kept), ask in zip(late, (False, True)):
            name = in_ws.name
            shutil.move(str(kept), str(in_ws))
            added = time.monotonic()
            expected = expected | {name}
            if ask:
                check_answers(expected, f"just after adding {name}")
            took = None
            while time.monotonic() - added < a.timeout:
                states = status(a.bin, env, outside)
                if states.get(name) == "current":
                    took = time.monotonic() - added
                    break
                time.sleep(min(a.poll, 5))
            if took is None:
                problems.append(f"{name}, added under the root, was not indexed: {states.get(name, 'not listed')}")
            added_at[name] = took
        check_answers(expected, "at the end")
    finally:
        server.close()

    report = {
        "repos": len(expected),
        "all_indexed_s": all_indexed,
        "added": added_at,
        "answer_checks": checks,
        "answers": answers,
        "problems": problems,
        "timeline": timeline,
    }
    (work / "gate.json").write_text(json.dumps(report, indent=1))
    print(f"\n## Workspace gate: {len(expected)} public repos\n")
    print(f"- every repo indexed and current without `index`: {'yes' if all_indexed is not None else 'NO'}"
          + (f" ({all_indexed:.0f} s wall, indicative)" if all_indexed is not None else ""))
    for (name, took), how in zip(added_at.items(), ("by the server alone", "while asked about")):
        print(f"- {name}, added under the root, noticed and indexed {how}: {'yes' if took is not None else 'NO'}"
              + (f" ({took:.0f} s wall, indicative)" if took is not None else ""))
    noted = sum(1 for x in answers if x["said"])
    print(f"- workspace answers checked: {checks} ({noted} while repos were waiting, each saying how many); repos left out silently: {'none' if not problems else 'see below'}")
    for p in problems:
        print(f"  - {p}")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
