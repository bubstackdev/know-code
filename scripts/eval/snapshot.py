"""`snapshot` and `diff`: a regression gate on answers.

A query set (TOML) names repos and knowcode commands. `snapshot` runs each with `--json --no-sync` and stores the
answer normalised (no build info, timings or row ids; lists sorted only where their order means nothing), one file
per query. A query that fails (an exit code it doesn't expect, a timeout, output that isn't JSON) makes the snapshot
invalid: every answer is still written, the failures are listed in `_meta.json`, and `snapshot` exits 1. `diff`
compares two snapshots query by query: exit 0 when every answer is the same, 1 when any differs, 2 when it refuses to
compare (either snapshot invalid, incomplete, or taken under another normalisation format).

Query set format:

    root = "~/Library/Caches/dev.bubstack.knowcode/corpus/public"   # repos are directories under it
    index = "repos"          # what `snapshot --index` indexes first: "repos" (those named) or "workspace" (all of root)
    config = "<dir>"         # optional: KNOWCODE_CONFIG_DIR for the run (default: an empty one in the cache);
                             # `snapshot --config <dir>` overrides it, to compare one query set under two configs

    [[query]]
    id = "flask-context-Flask"     # unique; the snapshot file name
    repo = "flask"                 # or repos = ["a", "b"] for a cross-repo question
    cmd = "context"                # a subcommand; "note list" for a nested one
    args = ["Flask"]               # `{root}` and `{repo}` expand to absolute paths
    ordered = ["/items"]           # optional: lists that keep their order though the command is in UNORDERED
    unordered = ["/items[]/tags"]  # optional: lists sorted though the command keeps order (`true`: every list)
    exit = 2                       # optional: an exit code that still carries the answer (default 0)
    scope = "workspace"            # optional: no --repo, run from the root: the answer spans every indexed repo
    text = true                    # optional: the command has no JSON form; the answer is its output, {"text": ...}
    drop = ["/index/bytes"]        # optional: fields that aren't part of the answer (sizes of the index itself)

Every list keeps its order unless the command is in UNORDERED or the query says otherwise: a ranking that changed is
a different answer, so order is compared by default and the exceptions are named.

The run gets its own config and data directories, so the user's config.toml (workspace roots, ignore rules), notes
and bot_authors never reach a snapshot.
"""

import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from common import ROOT, canon, expand, kc, kc_json, load_toml, normalize, repo_dir

# Commands whose lists carry no ranking (rows in file and line order, trees whose children are a set): their lists
# are sorted before comparing, so an order change alone isn't a difference. Every other command (locate, tests,
# coupled, hotspots, reviewers, find, search, ownership, impact, path, flow, at, overview, ...) keeps every list.
UNORDERED = {"context", "refs", "trace", "routes", "tables", "events", "deps", "unused", "rename"}

# Bumped whenever normalisation changes: snapshots taken under different rules aren't comparable.
FORMAT = 2


def load_queries(path):
    qs = load_toml(path)
    root = expand(qs.get("root", "."))
    seen = set()
    for q in qs["query"]:
        if q["id"] in seen:
            raise SystemExit(f"{path}: duplicate query id {q['id']}")
        seen.add(q["id"])
        q.setdefault("repos", [q["repo"]] if "repo" in q else [])
    return qs, root


def query_argv(q, root):
    dirs = [repo_dir(root, r) for r in q["repos"]]
    subst = {"root": str(root), "repo": str(dirs[0]) if dirs else ""}
    args = [a.format(**subst) for a in q.get("args", [])]
    argv = [*q["cmd"].split(), *args, "--json", "--no-sync"]
    if q.get("scope") != "workspace":
        for d in dirs:
            argv += ["--repo", str(d)]
    return argv


def order_rules(q):
    """(keep_order, ordered, unordered) for normalising the answer to `q`."""
    keep = q["cmd"].split()[0] not in UNORDERED
    unordered = q.get("unordered", ())
    if unordered is True:
        keep, unordered = False, ()
    return keep, set(q.get("ordered", ())), set(unordered)


def is_error(ans):
    """An answer the harness made up because the query failed (common.kc_json)."""
    return isinstance(ans, dict) and set(ans) == {"error", "exit"}


def drop(ans, path):
    """Removes the field at `path` (`/index/bytes`, `/items[]/size`) from `ans`, in place."""
    head, _, rest = path.lstrip("/").partition("/")
    each = head.endswith("[]")
    key = head[:-2] if each else head
    if not isinstance(ans, dict) or key not in ans:
        return
    if not rest and not each:
        del ans[key]
        return
    for v in ans[key] if each and isinstance(ans[key], list) else [ans[key]]:
        drop(v, "/" + rest)


def run_one(q, root, bin_, cache):
    # A workspace question runs from the root, which is no repo, so the answer spans every indexed repo.
    cwd = root if q.get("scope") == "workspace" else None
    exits = (0, q.get("exit", 0))
    argv = query_argv(q, root)
    if q.get("text"):
        # A command with no JSON form (`docs` prints Markdown): the answer is its text.
        code, out, err = kc(bin_, [a for a in argv if a != "--json"], cache, cwd=cwd)
        raw = {"text": out} if code in exits else {"error": (err.strip().splitlines() or ["(no stderr)"])[-1], "exit": code}
    else:
        raw = kc_json(bin_, argv, cache, cwd=cwd, exits=exits)
    if is_error(raw):
        return q["id"], raw
    for path in q.get("drop", ()):
        drop(raw, path)
    keep, ordered, unordered = order_rules(q)
    roots = [("<root>", str(root)), ("<cache>", str(cache)), ("~", str(Path.home()))]
    return q["id"], normalize(raw, roots, ordered, keep_order=keep, unordered=unordered)


def snapshot(a):
    qs, root = load_queries(a.queries)
    # Hotspot windows end now unless told otherwise; a snapshot pins them (to each repo's HEAD by default) so it
    # doesn't change from one day to the next. A query set may name a time instead: `now = "2026-10-13"`.
    os.environ["KNOWCODE_NOW"] = str(qs.get("now", "head"))
    queries = qs["query"]
    if a.only:
        queries = [q for q in queries if a.only in q["id"]]
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    cache = Path(a.cache).resolve()
    a.bin = Path(a.bin).resolve()  # a workspace query runs from the root
    if a.config:
        config = Path(a.config).expanduser().resolve()
    elif "config" in qs:
        config = expand(qs["config"])
    else:
        config = cache / "_config"
    config.mkdir(parents=True, exist_ok=True)
    os.environ["KNOWCODE_CONFIG_DIR"] = str(config)
    # Notes live in the user data directory: the run gets its own, so the user's notes never reach a snapshot.
    os.environ["KNOWCODE_DATA_DIR"] = str(cache / "_data")
    if a.index:
        if qs.get("index") == "workspace":
            targets = [["--workspace", str(root)]]
        else:
            targets = [[str(repo_dir(root, r))] for r in sorted({r for q in queries for r in q["repos"]})]
        for t in targets:
            code, _, err = kc(a.bin, ["index", *t, "--threads", str(a.threads)], cache, timeout=3600)
            if code != 0:
                raise SystemExit(f"index {' '.join(t)} failed: {err.strip()}")
    t0 = time.time()
    with ThreadPoolExecutor(a.jobs) as pool:
        results = list(pool.map(lambda q: run_one(q, root, a.bin, cache), queries))
    # Answers left from an earlier snapshot in the same directory would be compared as if this one gave them.
    old = list(out.glob("*.json"))
    if old and not (out / "_meta.json").exists():
        raise SystemExit(f"{out} holds JSON files but no snapshot; use an empty directory")
    for p in old:
        p.unlink()
    failed = []
    for qid, ans in results:
        if is_error(ans):
            failed.append(qid)
            print(f"{qid}: error (exit {ans['exit']}): {ans['error']}", file=sys.stderr)
        (out / f"{qid}.json").write_text(json.dumps(ans, indent=1, sort_keys=True, ensure_ascii=False) + "\n")
    _, ver, _ = kc(a.bin, ["--version"], cache)
    meta = {
        "binary": str(a.bin), "version": ver.strip(), "queries": str(a.queries), "config": str(config), "count": len(results),
        "errors": len(failed), "failed": failed, "valid": not failed, "format": FORMAT, "only": a.only,
        "taken": time.strftime("%Y-%m-%d %H:%M:%S"), "seconds": round(time.time() - t0, 1),
    }
    (out / "_meta.json").write_text(json.dumps(meta, indent=1) + "\n")
    print(f"{len(results)} answers in {out} ({len(failed)} errors), {meta['seconds']} s")
    if failed:
        print(f"snapshot INVALID: {len(failed)} queries failed, and `diff` refuses it", file=sys.stderr)
        sys.exit(1)


# Fields that name a row in an answer; two rows with the same values are the same row, changed.
IDENT = ("repo", "file", "line", "symbol", "qualified", "name", "path", "verb", "role", "table", "message", "via",
         "site", "kind", "context")


def ident(v):
    if isinstance(v, dict):
        key = tuple((k, canon(v[k])) for k in IDENT if k in v and not isinstance(v[k], (dict, list)))
        return key or None
    return None


def brief(v, width=140):
    """One line for a row: its identifying fields if it has them, else its JSON."""
    if isinstance(v, dict):
        k = ident(v)
        s = " ".join(f"{n}={json.loads(x)}" for n, x in k) if k else canon(v)
    else:
        s = canon(v)
    return s if len(s) <= width else s[: width - 1] + "…"


def diff_values(a, b, path, out):
    if canon(a) == canon(b):
        return
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a:
                out.append(f"  + {path}/{k}: {brief(b[k])}")
            elif k not in b:
                out.append(f"  - {path}/{k}: {brief(a[k])}")
            else:
                diff_values(a[k], b[k], f"{path}/{k}", out)
        return
    if isinstance(a, list) and isinstance(b, list):
        ca, cb = [canon(x) for x in a], [canon(x) for x in b]
        if sorted(ca) == sorted(cb):
            out.append(f"  ~ {path}: same {len(a)} rows, reordered")
            return
        rest_b = list(cb)
        gone = []
        for x, c in zip(a, ca):
            if c in rest_b:
                rest_b.remove(c)
            else:
                gone.append(x)
        added = [json.loads(c) for c in rest_b]
        # Pair a removed row with an added one naming the same thing, and show what changed in it.
        by_key = {}
        for x in added:
            by_key.setdefault(ident(x), []).append(x)
        for x in gone:
            k = ident(x)
            if k is not None and by_key.get(k):
                y = by_key[k].pop(0)
                added.remove(y)
                diff_values(x, y, f"{path}[{brief(x, 60)}]", out)
            else:
                out.append(f"  - {path}[]: {brief(x)}")
        for y in added:
            out.append(f"  + {path}[]: {brief(y)}")
        if len(a) != len(b):
            out.append(f"    {path}: {len(a)} -> {len(b)} rows")
        return
    out.append(f"  ~ {path}: {brief(a, 60)} -> {brief(b, 60)}")


def problems(d):
    """Why the snapshot in `d` can't be compared (empty when it can): no `_meta.json`, failed queries, answers
    missing, or another normalisation format."""
    meta_path = d / "_meta.json"
    if not meta_path.exists():
        return [f"{d}: no _meta.json (not a finished snapshot)"]
    meta = json.loads(meta_path.read_text())
    out = []
    if meta.get("format") != FORMAT:
        out.append(f"{d}: taken under normalisation format {meta.get('format', 1)}, this harness's is {FORMAT}: retake it")
    answers = {p.stem: json.loads(p.read_text()) for p in d.glob("*.json") if p.name != "_meta.json"}
    failed = sorted(set(meta.get("failed", ())) | {q for q, v in answers.items() if is_error(v)})
    if failed or meta.get("errors"):
        out.append(f"{d}: {max(len(failed), meta.get('errors', 0))} queries failed: {', '.join(failed[:10])}")
    if meta.get("count") != len(answers):
        out.append(f"{d}: {len(answers)} answers, but _meta.json says {meta.get('count')}")
    return out


def diff(a):
    da, db = Path(a.a), Path(a.b)
    bad = problems(da) + problems(db)
    if bad:
        for line in bad:
            print(line)
        print("\nrefusing to compare: an invalid snapshot can hide a regression")
        sys.exit(2)
    fa = {p.stem: p for p in da.glob("*.json") if p.name != "_meta.json"}
    fb = {p.stem: p for p in db.glob("*.json") if p.name != "_meta.json"}
    changed = 0
    for qid in sorted(set(fa) | set(fb)):
        if qid not in fb:
            print(f"{qid}: only in {da}")
            changed += 1
            continue
        if qid not in fa:
            print(f"{qid}: only in {db}")
            changed += 1
            continue
        va, vb = json.loads(fa[qid].read_text()), json.loads(fb[qid].read_text())
        lines = []
        diff_values(va, vb, "", lines)
        if lines:
            changed += 1
            print(f"{qid}: {len(lines)} difference{'s' * (len(lines) != 1)}")
            for line in lines[: a.max]:
                print(line)
            if len(lines) > a.max:
                print(f"  … {len(lines) - a.max} more")
    total = len(set(fa) | set(fb))
    print(f"\n{changed} of {total} queries differ; {total - changed} identical.")
    sys.exit(1 if changed else 0)


def add_parsers(sub):
    p = sub.add_parser("snapshot", help="run a query set and store normalised answers; exit 1 if any query failed")
    p.add_argument("--queries", default=str(ROOT / "eval" / "queries.toml"), help="query set (TOML)")
    p.add_argument("--bin", required=True, type=Path, help="the knowcode binary to ask")
    p.add_argument("--cache", required=True, help="KNOWCODE_CACHE_DIR for the run")
    p.add_argument("--out", required=True, help="directory for the answers")
    p.add_argument("--index", action="store_true", help="index the set's repos (or root, for index = \"workspace\") first")
    p.add_argument("--only", help="only queries whose id contains this")
    p.add_argument("--config", help="KNOWCODE_CONFIG_DIR for the run, over the query set's `config`")
    p.add_argument("--jobs", type=int, default=4)
    p.add_argument("--threads", type=int, default=4, help="parser threads for --index")
    p.set_defaults(func=snapshot)
    p = sub.add_parser("diff", help="compare two snapshots; exit 1 if any answer differs, 2 if either is invalid")
    p.add_argument("a")
    p.add_argument("b")
    p.add_argument("--max", type=int, default=12, help="difference lines shown per query")
    p.set_defaults(func=diff)
