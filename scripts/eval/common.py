"""Shared pieces of the evaluation harness: running knowcode, JSON paths, normalisation and statistics."""

import json
import math
import os
import random
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
BIN = ROOT / "target" / "release" / "knowcode"
CORPUS = Path.home() / "Library" / "Caches" / "dev.bubstack.knowcode" / "corpus"
# The GitLab group corpus (scripts/corpus.py gitlab), and its private eval data (query sets, datasets, snapshots,
# labels), which lives here, never in the repo.
GITLAB = CORPUS / os.environ.get("KNOWCODE_GITLAB_CORPUS", "gitlab")
EVAL_DIR = Path(os.environ.get("KNOWCODE_EVAL_DIR", f"{GITLAB}-eval"))


def kc(bin_, args, cache, cwd=None, timeout=600):
    """Runs knowcode with its own cache directory. Returns (exit code, stdout, stderr)."""
    env = dict(os.environ, KNOWCODE_CACHE_DIR=str(cache))
    try:
        out = subprocess.run([str(bin_), *args], env=env, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return 124, "", f"timed out after {timeout} s"
    return out.returncode, out.stdout, out.stderr


def kc_json(bin_, args, cache, cwd=None, timeout=600, exits=(0,)):
    """Runs a knowcode query with `--json`; returns the parsed answer, or {"error": ..., "exit": n} on failure.
    `exits` are the exit codes that still carry an answer (`rename` refusing a preview exits 2 with its JSON)."""
    code, out, err = kc(bin_, args, cache, cwd, timeout)
    if code not in exits:
        return {"error": (err.strip().splitlines() or ["(no stderr)"])[-1], "exit": code}
    try:
        return json.loads(out)
    except json.JSONDecodeError as e:
        return {"error": f"bad JSON: {e}", "exit": code}


def load_toml(path):
    return tomllib.loads(Path(path).read_text())


def expand(p):
    return Path(os.path.expandvars(os.path.expanduser(str(p))))


def repo_dir(root, name):
    """A repo named in a query set: an absolute path, or a directory name under the set's root."""
    p = expand(name)
    return p if p.is_absolute() else expand(root) / name


def json_path(obj, path):
    """Values at a path such as `items[].affected[].file`: `[]` walks every element of a list. Missing keys yield
    nothing, so one path serves answers whose rows differ in shape."""
    cur = [obj]
    for part in path.split("."):
        each = part.endswith("[]")
        key = part[:-2] if each else part
        nxt = []
        for v in cur:
            if key:
                if not isinstance(v, dict) or key not in v:
                    continue
                v = v[key]
            if each:
                if isinstance(v, list):
                    nxt.extend(v)
            else:
                nxt.append(v)
        cur = nxt
    return cur


# Keys whose values change between runs or builds without the answer changing (`generation` counts index writes).
VOLATILE_KEYS = {"knowcode", "freshness", "stats", "elapsed", "duration", "generation"}


def is_volatile(key, value):
    if key in VOLATILE_KEYS or key == "ms" or key.endswith("_ms"):
        return True
    # Symbol row ids differ between two cold indexes of the same tree.
    return (key == "id" or key.endswith("_id")) and isinstance(value, int)


def canon(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"))


def normalize(obj, roots=(), ordered=(), path="", keep_order=False, unordered=()):
    """Drops volatile fields and row ids, rewrites absolute paths under `roots` to `<root>`, rounds floats to six
    significant digits, and sorts every list whose order is not part of the answer. With `keep_order`, every list
    keeps its order except those at the paths in `unordered`; without it, every list is sorted except those at the
    paths in `ordered`. Paths look like `/items` and `/items[]/map`."""
    if isinstance(obj, dict):
        return {
            k: normalize(v, roots, ordered, f"{path}/{k}", keep_order, unordered)
            for k, v in sorted(obj.items())
            if not is_volatile(k, v)
        }
    if isinstance(obj, list):
        items = [normalize(v, roots, ordered, path + "[]", keep_order, unordered) for v in obj]
        keep = path not in unordered if keep_order else path in ordered
        return items if keep else sorted(items, key=canon)
    if isinstance(obj, str):
        for name, r in roots:
            if r and r in obj:
                obj = obj.replace(r, name)
        return obj
    if isinstance(obj, float):
        # Ratios summed in a different order differ in the last bits between two cold indexes.
        return float(f"{obj:.6g}")
    return obj


# Mirrors kc_store::is_test_path (crates/kc-store/src/lib.rs); keep the two in step.
TEST_DIRS = {"Tests", "tests", "test", "Test", "__tests__", "testing", "UITests"}


def is_test_path(path):
    name = path.rsplit("/", 1)[-1]
    stem = name.split(".", 1)[0]
    return (
        any(c in TEST_DIRS or c.endswith("Tests") for c in path.split("/"))
        or stem.endswith(("Tests", "Test", "_test", "_tests"))
        or stem.startswith("test_")
        or stem == "conftest"
        or ".spec." in name
        or ".test." in name
    )


# Extensions knowcode extracts symbols from (kc-extract's languages, plus Angular templates).
SOURCE_EXTS = {".swift", ".py", ".rs", ".cs", ".ts", ".tsx", ".html"}


def is_source(path):
    return os.path.splitext(path)[1].lower() in SOURCE_EXTS


def wilson(k, n, z=1.96):
    """Wilson score interval for k successes in n trials."""
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def bootstrap(values, iters=2000, seed=0, groups=None):
    """95% percentile interval of the mean. With `groups` (one label per value), resamples whole groups (repos), so
    tasks from one repo don't count as independent."""
    if not values:
        return (0.0, 0.0)
    rng = random.Random(seed)
    if groups is None:
        groups = list(range(len(values)))
    by = {}
    for g, v in zip(groups, values):
        by.setdefault(g, []).append(v)
    keys = list(by)
    means = []
    for _ in range(iters):
        s, n = 0.0, 0
        for _ in keys:
            vs = by[rng.choice(keys)]
            s += sum(vs)
            n += len(vs)
        means.append(s / n)
    means.sort()
    return (means[int(0.025 * iters)], means[int(0.975 * iters) - 1])


def pct(x):
    return f"{100 * x:.1f}%"
