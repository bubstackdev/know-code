#!/usr/bin/env python3
"""The worktree gate: N worktrees of one public repo share an index base and answer as N separate indexes do.

Usage: scripts/worktree-gate.py [--bin PATH] [--list FILE] [--work DIR] [-n N] [--repo NAME ...] [--json FILE]

For each repo in --list (default scripts/worktree-gate/repos.json: public repos at pinned commits):

1. Clones it under <work>/src (reused across runs) and makes N worktrees of it under <work>/wt, at the pinned commit
   and a few commits before it, some with uncommitted changes: a declaration appended to a tracked file, a tracked file
   deleted, an untracked file added (see PLAN). Nothing is written into any other checkout.
2. Shared: one cache. The first worktree (clean at the pinned commit) is indexed and publishes a base; the others are
   indexed after it, each starting from the nearest base.
3. Separate: each worktree indexed alone in a cache of its own, with shared bases off (`cache.shared_bases = false`).
   A control indexes the first worktree twice more, alone: the JSON fields that differ between those two cold indexes
   of one tree (timings, generations, row ids) are listed and left out of every comparison. Nothing else is.
4. Every worktree runs the repo's fixed query set (`knowcode <query> --json`) in both caches; the answers must match.
5. Freshness, in every worktree and both caches: a new untracked file (found by the next query), then same-size edits
   with the modification time restored, of a tracked file and of the untracked one. The first is asked without
   `KNOWCODE_VERIFY` (both caches must answer alike) and with `KNOWCODE_VERIFY=1` (the edit must show); the second
   without it, before and after a `context` call (whose source read checks the file's hash; the edit must show after).
6. Then N clean worktrees of the pinned commit (--at-once, 4 by default) are indexed at the same moment in a cache with
   no base: one must build the base while the others wait for it and clone it, and their answers must match separate
   indexes'.
7. Reports instructions retired by the `index` commands (macOS `/usr/bin/time -l`) and the disk each cache holds:
   the bytes no other file shares (APFS `ATTR_CMNEXT_PRIVATESIZE`), with shared blocks counted once, under the base.

Prints a Markdown report; --json writes the details. Exit status 1 when a check fails. Instruction counts hold on a
loaded machine; no wall time is reported.
"""

import argparse
import ctypes
import ctypes.util
import json
import os
import re
import shlex
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_LIST = ROOT / "scripts" / "worktree-gate" / "repos.json"
# Each worktree by position: how many commits before the pinned one it is at, and whether it has uncommitted changes.
# The first publishes the base; the third is another clean checkout of the base's commit (a job's fresh worktree).
PLAN = [(0, False), (0, True), (0, False), (3, False), (15, True), (1, True), (7, False), (30, True)]
MAX_N = len(PLAN)


def declaration(ext, name):
    return {
        "swift": f"\nfunc {name}() {{}}\n",
        "rs": f"\npub fn {name}() {{}}\n",
        "py": f"\ndef {name}():\n    pass\n",
        "ts": f"\nexport function {name}() {{}}\n",
        "cs": f"\nstatic class {name}Holder {{ static void {name}() {{}} }}\n",
    }[ext]


def git(cwd, *args, check=True):
    return subprocess.run(["git", "-C", str(cwd), *args], check=check, capture_output=True, text=True).stdout.strip()


def clone(repo, dest):
    if (dest / ".git").exists() and git(dest, "rev-parse", "HEAD", check=False) == repo["commit"]:
        return "reused"
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    git(dest, "init", "-q")
    git(dest, "remote", "add", "origin", repo["url"])
    git(dest, "fetch", "-q", "--depth", str(max(o for o, _ in PLAN) + 20), "origin", repo["commit"])
    git(dest, "-c", "advice.detachedHead=false", "checkout", "-q", "FETCH_HEAD")
    return "cloned"


def make_worktrees(repo, src, wt_dir, n):
    """N worktrees, made afresh: (path, commit, dirty)."""
    for line in git(src, "worktree", "list", "--porcelain").splitlines():
        if line.startswith("worktree ") and Path(line[9:]).resolve().is_relative_to(wt_dir.resolve()):
            git(src, "worktree", "remove", "--force", line[9:], check=False)
    git(src, "worktree", "prune")
    if wt_dir.exists():
        shutil.rmtree(wt_dir)
    wt_dir.mkdir(parents=True)
    out = []
    for i in range(n):
        offset, dirty = PLAN[i]
        commit = git(src, "rev-parse", f"{repo['commit']}~{offset}")
        path = wt_dir / f"w{i}"
        git(src, "worktree", "add", "-q", "--detach", str(path), commit)
        if dirty:
            edit = path / repo["edit"]
            if edit.exists():
                with open(edit, "a") as f:
                    f.write(declaration(repo["ext"], f"gateProbe{i}"))
            (path / repo["delete"]).unlink(missing_ok=True)
            (path / repo["probe_dir"] / f"GateProbe{i}.{repo['ext']}").write_text(declaration(repo["ext"], f"gateUntracked{i}"))
        out.append({"path": path, "commit": commit, "dirty": dirty, "index": i})
    return out


def env_for(work, cache, shared=True):
    cfg = work / "config" / cache.name
    cfg.mkdir(parents=True, exist_ok=True)
    (cfg / "config.toml").write_text(f"[cache]\nmax_size = 0\nshared_bases = {'true' if shared else 'false'}\n[update]\ncheck = false\n")
    env = kc_env.knowcode_env(work / "env" / cache.name, cache=cache, config=cfg, data=work / "data" / cache.name)
    env.pop("KNOWCODE_VERIFY", None)
    return env


INSTRUCTIONS = re.compile(r"^\s*(\d+)\s+instructions retired", re.M)


def index(binary, root, env):
    """`knowcode index --phases ROOT`: instructions retired by the process, by the phases a query needs before it can
    answer (everything but `derived.*`), and by the derived data (the file ranking and the areas, which `index`
    computes ahead and a query otherwise computes when it first needs them); and its JSON row."""
    return index_finish(root, index_start(binary, root, env))


def index_start(binary, root, env):
    timed = ["/usr/bin/time", "-l"] if sys.platform == "darwin" else []
    return subprocess.Popen([*timed, str(binary), "index", "--phases", "--json", str(root)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def index_finish(root, p):
    out, err = p.communicate()
    if p.returncode != 0:
        raise SystemExit(f"index {root} failed: {err[-2000:]}")
    m = INSTRUCTIONS.search(err)
    row = json.loads(out)["repos"][0]
    phases = row.get("phases", [])
    # Instructions are counted on macOS; elsewhere the phases give CPU time only.
    derived = sum(ph.get("instructions", 0) for ph in phases if ph["name"].startswith("derived."))
    answerable = sum(ph.get("instructions", 0) for ph in phases if not ph["name"].startswith("derived."))
    cpu_ms = sum(ph.get("cpu_ms", 0) for ph in phases)
    return {"process": int(m.group(1)) if m else None, "answerable": answerable, "derived": derived, "cpu_ms": cpu_ms}, row


def ask(binary, root, env, query, verify=False):
    env = dict(env)
    if verify:
        env["KNOWCODE_VERIFY"] = "1"
    p = subprocess.run([str(binary), *shlex.split(query), "--json"], cwd=root, env=env, capture_output=True)
    return p.stdout + (b"\n[exit %d]" % p.returncode if p.returncode else b"")


# --- comparing answers ----------------------------------------------------------------------------------------------


def walk(value, path=()):
    if isinstance(value, dict):
        for k, v in value.items():
            yield from walk(v, path + (k,))
    elif isinstance(value, list):
        for v in value:
            yield from walk(v, path + ("*",))
    else:
        yield path, value


def parsed(answer):
    try:
        return json.loads(answer)
    except ValueError:
        return None


def differing(a, b):
    """The paths (array positions as `*`) whose leaf values differ between two answers; `None` when either isn't JSON."""
    ja, jb = parsed(a), parsed(b)
    if ja is None or jb is None:
        return None
    la, lb = sorted(walk(ja), key=repr), sorted(walk(jb), key=repr)
    from collections import Counter
    ca, cb = Counter((p, json.dumps(v)) for p, v in la), Counter((p, json.dumps(v)) for p, v in lb)
    return {p for p, _ in (ca - cb) + (cb - ca)}


AGE = re.compile(r"\b\d+ (?:second|minute|hour|day)s? earlier\b")


def per_index(path):
    """Fields that name or measure one index rather than the code: row ids (`id`), cursors (they carry the
    generation), the generation, timings (`*_ms`), and a search's work counters (`stats.candidates`,
    `stats.bytes_read`: how many files the content index's postings named, which depends on its merge history as in
    any index updated in place; the matches are verified against the files either way)."""
    last = path[-1] if path else ""
    return (last in ("id", "cursor", "generation", "ms") or last.endswith("_cursor") or last.endswith("_ms")
            or (len(path) >= 2 and path[-2] == "stats" and last in ("candidates", "bytes_read")))


def masked(answer, volatile):
    """The answer with every leaf at a volatile or per-index path replaced, re-serialized canonically; the raw bytes
    when it isn't JSON."""
    j = parsed(answer)
    if j is None:
        return answer

    def go(v, path):
        if isinstance(v, dict):
            return {k: go(x, path + (k,)) for k, x in v.items()}
        if isinstance(v, list):
            return [go(x, path + ("*",)) for x in v]
        if path in volatile or per_index(path):
            return "<volatile>"
        # How long ago the index was in a state an answer drew on: the clock, not the code.
        return AGE.sub("<age> earlier", v) if isinstance(v, str) else v

    return json.dumps(go(j, ()), sort_keys=True).encode()


# --- disk -----------------------------------------------------------------------------------------------------------


class _AttrList(ctypes.Structure):
    _fields_ = [("bitmapcount", ctypes.c_ushort), ("reserved", ctypes.c_ushort), ("commonattr", ctypes.c_uint32),
                ("volattr", ctypes.c_uint32), ("dirattr", ctypes.c_uint32), ("fileattr", ctypes.c_uint32),
                ("forkattr", ctypes.c_uint32)]


_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True) if sys.platform == "darwin" else None


def private_size(path):
    """Bytes of `path` no other file shares (APFS), else its length."""
    if _libc is None:
        return path.stat().st_size
    attrs = _AttrList(5, 0, 0, 0, 0, 0, 0x8)  # ATTR_CMNEXT_PRIVATESIZE
    buf = ctypes.create_string_buffer(16)
    if _libc.getattrlist(str(path).encode(), ctypes.byref(attrs), buf, 16, 0x20) != 0:  # FSOPT_ATTR_CMN_EXTENDED
        return path.stat().st_size
    return int.from_bytes(buf.raw[4:12], "little")


def disk(cache):
    """Bytes the cache's indexes and bases hold on disk: each file's own bytes, plus the blocks a base still shares
    with its clones, once. Also their apparent size."""
    own = apparent = 0
    for sub in ("repos", "bases"):
        for f in sorted((cache / sub).glob("*")) if (cache / sub).is_dir() else []:
            if not f.is_file() or f.suffix in (".lock", ".use"):
                continue
            size = f.stat().st_size
            mine = private_size(f)
            apparent += size
            own += mine
            if sub == "bases" and f.suffix == ".sqlite":
                own += size - mine
    return {"bytes": own, "apparent": apparent}


def cloned_from(cache, root):
    for db in (cache / "repos").glob(f"{root.name}-*.sqlite"):
        con = sqlite3.connect(str(db))
        try:
            meta = dict(con.execute("SELECT key, value FROM meta WHERE key IN ('root', 'base.from')"))
        finally:
            con.close()
        if meta.get("root") == str(root):
            return meta.get("base.from")
    return None


# --- freshness ------------------------------------------------------------------------------------------------------


def same_size_edit(path, old, new):
    st = os.stat(path)
    text = path.read_text()
    assert old in text and len(old) == len(new), (path, old, new)
    path.write_text(text.replace(old, new, 1))
    os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns))
    assert os.stat(path).st_size == st.st_size and os.stat(path).st_mtime_ns == st.st_mtime_ns


def found(answer, name):
    j = parsed(answer)
    return bool(j) and any(isinstance(i, dict) and i.get("name") == name for i in j.get("items", []))


def freshness(binary, repo, wt, envs):
    """Runs the freshness cases in a worktree, asking each cache after each step: {mode: [(label, answer)]}, and the
    checks each answer must pass, [(mode, check, ok)]."""
    root, i = wt["path"], wt["index"]
    ext = repo["ext"]
    answers = {m: [] for m in envs}
    checks = []

    def ask_all(label, query, verify=False, expect=None):
        for m, env in envs.items():
            a = ask(binary, root, env, query, verify=verify)
            answers[m].append((label, a))
            if expect:
                checks.append((m, expect, found(a, query.split()[-1])))

    probe = root / repo["probe_dir"] / f"GateFresh{i}.{ext}"
    probe.write_text(declaration(ext, "gateFreshNew"))
    ask_all("untracked file added: find gateFreshNew", "find gateFreshNew", expect="new untracked file found")
    old, new = repo["rename"]
    third = new[:-1] + ("Q" if new[-1] != "Q" else "R")
    for label, path, names in (("tracked", root / repo["edit"], (old, new, third)), ("untracked", probe, ("gateFreshNew", "gateFreshOne", "gateFreshTwo"))):
        a, b, c = names
        # Seen with KNOWCODE_VERIFY=1, which hashes every file.
        same_size_edit(path, a, b)
        ask_all(f"{label} same-size edit, mtime kept: find {b}", f"find {b}")
        ask_all(f"{label} same-size edit, mtime kept, KNOWCODE_VERIFY=1: find {b}", f"find {b}", verify=True,
                expect=f"{label} same-size edit seen with KNOWCODE_VERIFY=1")
        # Seen without it once a source read finds the file's hash moved (`context` checks it before quoting).
        same_size_edit(path, b, c)
        ask_all(f"{label} second same-size edit, mtime kept: find {c}", f"find {c}")
        ask_all(f"{label} second same-size edit: context {b}", f"context {b}")
        ask_all(f"{label} second same-size edit, after context: find {c}", f"find {c}", expect=f"{label} same-size edit seen without KNOWCODE_VERIFY after a source read")
    # Undone, with a new mtime, so the worktree is as it was for whatever runs next.
    probe.unlink()
    edit = root / repo["edit"]
    st = os.stat(edit)
    edit.write_text(edit.read_text().replace(third, old, 1))
    os.utime(edit, ns=(st.st_atime_ns, st.st_mtime_ns + 1_000_000))
    return answers, checks


# --- the gate -------------------------------------------------------------------------------------------------------


def gate_repo(binary, repo, work, n, at_once):
    src = work / "src" / repo["name"]
    print(f"## {repo['name']} at {repo['commit'][:12]}: {clone(repo, src)}", file=sys.stderr)
    wts = make_worktrees(repo, src, work / "wt" / repo["name"], n)
    caches = work / "caches" / repo["name"]
    if caches.exists():
        shutil.rmtree(caches)
    result = {"repo": repo["name"], "commit": repo["commit"], "worktrees": [], "failures": []}

    # Control: two cold indexes of one tree. What differs between them is not a difference a shared base makes.
    control = []
    for k in range(2):
        cache = caches / f"control{k}"
        env = env_for(work, cache, shared=False)
        index(binary, wts[0]["path"], env)
        control.append([ask(binary, wts[0]["path"], env, q) for q in repo["queries"]])
    volatile = set()
    for a, b in zip(*control):
        if masked(a, set()) == masked(b, set()):
            continue
        d = differing(a, b)
        if d is None and a != b:
            result["failures"].append("control: a non-JSON answer differs between two cold indexes")
        volatile |= {p for p in d or set() if not per_index(p)}
    result["volatile"] = sorted(".".join(p) for p in volatile)

    shared_cache = caches / "shared"
    shared_env = env_for(work, shared_cache, shared=True)
    totals = {"shared": {}, "separate": {}}
    for wt in wts:
        row = {"worktree": wt["path"].name, "commit": wt["commit"][:12], "dirty": wt["dirty"]}
        ins, rep = index(binary, wt["path"], shared_env)
        row["shared"] = {"instructions": ins, "parsed": rep["report"]["parsed"], "touched": rep["report"]["touched"],
                         "base": cloned_from(shared_cache, wt["path"])}
        cache = caches / f"separate-{wt['path'].name}"
        env = env_for(work, cache, shared=False)
        ins2, rep2 = index(binary, wt["path"], env)
        row["separate"] = {"instructions": ins2, "parsed": rep2["report"]["parsed"]}
        for mode, i in (("shared", ins), ("separate", ins2)):
            for k, v in i.items():
                totals[mode][k] = totals[mode].get(k, 0) + (v or 0)
        wt["separate_env"] = env
        result["worktrees"].append(row)
    result["disk_indexed"] = disk_both(caches, shared_cache, wts)

    for wt, row in zip(wts, result["worktrees"]):
        queries = list(repo["queries"])
        if wt["dirty"]:
            queries += [f"find gateProbe{wt['index']}", f"find gateUntracked{wt['index']}"]
        mismatches = []
        def keep(label, a, b):
            (work / "diffs").mkdir(exist_ok=True)
            stem = f"{repo['name']}-{wt['path'].name}-{re.sub(r'[^A-Za-z0-9]+', '_', label)[:80]}"
            (work / "diffs" / f"{stem}.shared.json").write_bytes(a)
            (work / "diffs" / f"{stem}.separate.json").write_bytes(b)

        for q in queries:
            a, b = ask(binary, wt["path"], shared_env, q), ask(binary, wt["path"], wt["separate_env"], q)
            if masked(a, volatile) != masked(b, volatile):
                mismatches.append(q)
                keep(q, a, b)
        # Freshness: each edit made once, every cache asked after it.
        fresh_answers, checks = freshness(binary, repo, wt, {"shared": shared_env, "separate": wt["separate_env"]})
        fresh_mismatch = []
        for (la, a), (_, b) in zip(fresh_answers["shared"], fresh_answers["separate"]):
            if masked(a, volatile) != masked(b, volatile):
                fresh_mismatch.append(la)
                keep(la, a, b)
        row["queries"] = len(queries)
        row["mismatches"] = mismatches
        row["freshness"] = {"mismatches": fresh_mismatch, "checks": [[m, k, ok] for m, k, ok in checks]}
        for q in mismatches:
            result["failures"].append(f"{wt['path'].name}: `{q}` differs from a separate index")
        for la in fresh_mismatch:
            result["failures"].append(f"{wt['path'].name}: freshness `{la}` differs from a separate index")
        for m, k, ok in checks:
            if not ok:
                result["failures"].append(f"{wt['path'].name} ({m}): {k} failed")
        if wt["index"] > 0 and not row["shared"]["base"]:
            result["failures"].append(f"{wt['path'].name}: not started from a base")

    result["instructions"] = totals
    result["disk_end"] = disk_both(caches, shared_cache, wts)
    if at_once:
        result["at_once"] = concurrent(binary, repo, work, at_once, volatile)
        result["failures"] += result["at_once"]["failures"]
    return result


def concurrent(binary, repo, work, k, volatile):
    """k clean worktrees of the pinned commit, indexed at once in one cache that has no base yet: one claims the base
    and builds it, the others wait for it and clone it. Against k separate indexes, and their answers compared."""
    src = work / "src" / repo["name"]
    wt_dir = work / "wt" / f"{repo['name']}-at-once"
    for line in git(src, "worktree", "list", "--porcelain").splitlines():
        if line.startswith("worktree ") and Path(line[9:]).resolve().is_relative_to(wt_dir.resolve()):
            git(src, "worktree", "remove", "--force", line[9:], check=False)
    git(src, "worktree", "prune")
    if wt_dir.exists():
        shutil.rmtree(wt_dir)
    wt_dir.mkdir(parents=True)
    roots = []
    for i in range(k):
        path = wt_dir / f"job{i}"
        git(src, "worktree", "add", "-q", "--detach", str(path), repo["commit"])
        roots.append(path)
    caches = work / "caches" / repo["name"]
    cache = caches / "at-once"
    env = env_for(work, cache, shared=True)
    procs = [index_start(binary, r, env) for r in roots]
    rows = []
    for r, p in zip(roots, procs):
        ins, row = index_finish(r, p)
        rows.append({"worktree": r.name, "instructions": ins, "parsed": row["report"]["parsed"], "base": cloned_from(cache, r)})
    sep = []
    for r in roots:
        sep_env = env_for(work, caches / f"at-once-separate-{r.name}", shared=False)
        ins, row = index(binary, r, sep_env)
        sep.append((ins, sep_env))
    failures = []
    for r, row, (_, sep_env) in zip(roots, rows, sep):
        bad = []
        for q in repo["queries"]:
            a, b = ask(binary, r, env, q), ask(binary, r, sep_env, q)
            if masked(a, volatile) != masked(b, volatile):
                bad.append(q)
                (work / "diffs").mkdir(exist_ok=True)
                stem = f"{repo['name']}-{r.name}-{re.sub(r'[^A-Za-z0-9]+', '_', q)[:80]}"
                (work / "diffs" / f"{stem}.shared.json").write_bytes(a)
                (work / "diffs" / f"{stem}.separate.json").write_bytes(b)
        row["mismatches"] = bad
        failures += [f"at once, {r.name}: `{q}` differs from a separate index" for q in bad]
    # The one that built the base adopted it after: it is the one that parsed files.
    built = [row for row in rows if row["parsed"] > 0]
    if len(built) != 1:
        failures.append(f"at once: {len(built)} of {k} worktrees indexed cold, where one should build the base")
    total = lambda xs: {key: sum((x[key] or 0) for x in xs) for key in ("process", "answerable", "derived", "cpu_ms")}
    return {"k": k, "rows": rows, "shared": total([row["instructions"] for row in rows]), "separate": total([ins for ins, _ in sep]),
            "failures": failures}


def disk_both(caches, shared_cache, wts):
    out = {"shared": disk(shared_cache), "separate": {"bytes": 0, "apparent": 0}}
    for wt in wts:
        d = disk(caches / f"separate-{wt['path'].name}")
        for k in d:
            out["separate"][k] += d[k]
    return out


def report(results, n):
    out = [f"# Worktree gate: {n} worktrees per repo on a shared base, against {n} separate indexes", "",
           "Instructions retired by `knowcode index --phases`: *answerable* is what a query needs before it can answer "
           "(open, discovery, stat, parse, write, content index, and for the shared cache cloning and publishing the "
           "base); *derived* is the file ranking and the areas, which `index` computes ahead and a query otherwise "
           "computes when it first needs them. Disk is the bytes each cache's files hold of their own (APFS private "
           "size), a base's shared blocks counted once.", ""]
    m = lambda x: f"{x / 1e6:,.0f}M"
    ratio = lambda a, b: f"{a / b:.2f}x" if b else "-"
    for r in results:
        out += [f"## {r['repo']} at {r['commit'][:12]}", ""]
        out += ["| worktree | commit | dirty | base | parsed (shared / separate) | answerable (shared / separate) | derived (shared / separate) | checks | mismatches |",
                "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
        for w in r["worktrees"]:
            base = (w["shared"]["base"] or "-").rsplit("-", 1)[-1][:12].replace(".sqlite", "")
            fm = len(w["mismatches"]) + len(w["freshness"]["mismatches"])
            sh, se = w["shared"]["instructions"], w["separate"]["instructions"]
            out.append(f"| {w['worktree']} | {w['commit']} | {'yes' if w['dirty'] else 'no'} | {base} | {w['shared']['parsed']} / {w['separate']['parsed']} | "
                       f"{m(sh['answerable'])} / {m(se['answerable'])} | {m(sh['derived'])} / {m(se['derived'])} | "
                       f"{w['queries']} queries, {len(w['freshness']['checks'])} freshness | {fm} |")
        t, di, de = r["instructions"], r["disk_indexed"], r["disk_end"]
        out += ["", f"- Instructions retired for all {n}, answerable: shared {m(t['shared']['answerable'])}, separate {m(t['separate']['answerable'])} "
                f"({ratio(t['shared']['answerable'], t['separate']['answerable'])}); with derived data: shared {m(t['shared']['answerable'] + t['shared']['derived'])}, "
                f"separate {m(t['separate']['answerable'] + t['separate']['derived'])} ({ratio(t['shared']['answerable'] + t['shared']['derived'], t['separate']['answerable'] + t['separate']['derived'])}); "
                f"whole processes: shared {m(t['shared']['process'])}, separate {m(t['separate']['process'])} ({ratio(t['shared']['process'], t['separate']['process'])}).",
                f"- Disk after indexing: shared {di['shared']['bytes']:,} bytes ({di['shared']['apparent']:,} apparent), separate {di['separate']['bytes']:,} "
                f"({ratio(di['shared']['bytes'], di['separate']['bytes'])}). After every query and freshness case: shared {de['shared']['bytes']:,}, separate "
                f"{de['separate']['bytes']:,} ({ratio(de['shared']['bytes'], de['separate']['bytes'])}).",
                f"- Fields left out besides per-index ones (they differed between two cold indexes of one tree): {', '.join(r['volatile']) or 'none'}.",
                f"- Failures: {len(r['failures'])}", ""]
        out += [f"  - {f}" for f in r["failures"]]
        out.append("")
        if "at_once" in r:
            a = r["at_once"]
            out += [f"### {a['k']} clean worktrees of the commit indexed at once, no base yet", "",
                    "| worktree | parsed | started from | instructions (process) | mismatches |", "| --- | --- | --- | --- | --- |"]
            for row in a["rows"]:
                base = "built and published it" if row["parsed"] else (row["base"] or "-").rsplit("-", 1)[-1].replace(".sqlite", "")[:12]
                out.append(f"| {row['worktree']} | {row['parsed']} | {base} | {m(row['instructions']['process'] or 0)} | {len(row['mismatches'])} |")
            out += ["", f"- Instructions retired, whole processes: at once on one cache {m(a['shared']['process'])}, {a['k']} separate indexes "
                    f"{m(a['separate']['process'])} ({ratio(a['shared']['process'], a['separate']['process'])}); answerable {m(a['shared']['answerable'])} "
                    f"against {m(a['separate']['answerable'])}, derived {m(a['shared']['derived'])} against {m(a['separate']['derived'])}.", ""]
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--bin", type=Path, default=ROOT / "target" / "release" / "knowcode")
    ap.add_argument("--list", type=Path, default=DEFAULT_LIST)
    ap.add_argument("--work", type=Path, required=True, help="scratch directory: clones, worktrees, caches")
    ap.add_argument("-n", type=int, default=5, help=f"worktrees per repo (at most {MAX_N})")
    ap.add_argument("--repo", action="append", help="only these repos (by name)")
    ap.add_argument("--at-once", type=int, default=4, help="clean worktrees of the commit indexed at once in a cache with no base (0: none)")
    ap.add_argument("--json", type=Path)
    args = ap.parse_args()
    if not 2 <= args.n <= MAX_N:
        ap.error(f"-n must be from 2 to {MAX_N}")
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    repos = json.loads(args.list.read_text())["repos"]
    if args.repo:
        repos = [r for r in repos if r["name"] in args.repo]
    results = [gate_repo(args.bin.resolve(), r, work, args.n, args.at_once) for r in repos]
    print(report(results, args.n))
    if args.json:
        args.json.write_text(json.dumps(results, indent=2, default=str))
    sys.exit(1 if any(r["failures"] for r in results) else 0)


if __name__ == "__main__":
    main()
