#!/usr/bin/env python3
"""Materialize the search test corpus outside the workspace.

Usage:
  scripts/corpus.py fetch            # shallow-clone eval/corpus.toml repos at their pinned commits
  scripts/corpus.py pins             # check out the eval pins (eval/search.toml) of local repos
  scripts/corpus.py path SPEC        # print the directory for SPEC (`name` or `repo@sha`)
  scripts/corpus.py gitlab [--jobs N]  # shallow-clone or refresh a GitLab group corpus (KNOWCODE_GITLAB_HOST; `glab`)

Everything lives under ~/Library/Caches/dev.bubstack.knowcode/corpus/:
- public/<name>: a public repo at the commit in eval/corpus.toml (`git fetch --depth 1 <url> <sha>`),
- pins/<sha12>/<repo>: a local workspace repo at a pinned commit, cloned with `git clone --shared` so the source
  repo is only read, never written,
- <corpus>/<name>, <corpus> being $KNOWCODE_GITLAB_CORPUS (default `gitlab`): a project on the GitLab at
  $KNOWCODE_GITLAB_HOST (non-archived, group-owned, visible to you, including internal projects you aren't a member
  of; `glab` must be logged in there) at its default branch, cloned with history back to HISTORY_DAYS so hotspots'
  365-day window is complete. <corpus>/manifest.json records each project's path and commit. Index it with
  KNOWCODE_CACHE_DIR set to its own cache (`<corpus>-index` beside the corpus) so it stays out of the default
  workspace: `KNOWCODE_CACHE_DIR=$(scripts/corpus.py path gitlab-cache) knowcode index --workspace
  $(scripts/corpus.py path gitlab)`.
"""

import json
import os
import subprocess
import sys
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = Path.home() / "Library" / "Caches" / "dev.bubstack.knowcode" / "corpus"
WORKSPACE = Path.home() / "Code" / "bubstackdev"
# The GitLab group corpus: its directory under CORPUS, and its index cache beside CORPUS.
GITLAB_NAME = os.environ.get("KNOWCODE_GITLAB_CORPUS", "gitlab")
GITLAB = CORPUS / GITLAB_NAME
GITLAB_CACHE = CORPUS.parent / f"{GITLAB_NAME}-index"
HISTORY_DAYS = 400


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


def public_repos():
    return tomllib.loads((ROOT / "eval" / "corpus.toml").read_text())["repo"]


def path(spec):
    """`name` (a public corpus repo) or `repo@sha` (a workspace repo at a pin)."""
    if spec in ("gitlab", "gitlab-cache"):
        return GITLAB if spec == "gitlab" else GITLAB_CACHE
    if "@" in spec:
        repo, sha = spec.split("@", 1)
        return CORPUS / "pins" / sha[:12] / repo
    return CORPUS / "public" / spec


def head(d):
    try:
        return git("rev-parse", "HEAD", cwd=d)
    except subprocess.CalledProcessError:
        return None


def fetch():
    for r in public_repos():
        d = path(r["name"])
        if d.exists() and head(d) == r["sha"]:
            print(f"ok      {r['name']} {r['sha'][:12]}")
            continue
        d.mkdir(parents=True, exist_ok=True)
        if not (d / ".git").exists():
            git("init", "-q", cwd=d)
        git("fetch", "-q", "--depth", "1", r["url"], r["sha"], cwd=d)
        git("checkout", "-q", "--detach", "FETCH_HEAD", cwd=d)
        print(f"fetched {r['name']} {head(d)[:12]}")


def pin(spec):
    repo, sha = spec.split("@", 1)
    d = path(spec)
    if d.exists() and (head(d) or "").startswith(sha):
        return d
    d.parent.mkdir(parents=True, exist_ok=True)
    if not d.exists():
        git("clone", "-q", "--shared", "--no-checkout", str(WORKSPACE / repo), str(d))
    git("checkout", "-q", "--detach", sha, cwd=d)
    return d


def pins():
    specs = set()
    for q in tomllib.loads((ROOT / "eval" / "search.toml").read_text())["query"]:
        specs.update(s for s in q["repos"] if "@" in s)
    for s in sorted(specs):
        print(f"ok      {s} -> {pin(s)}")


def gitlab_projects():
    host = os.environ.get("KNOWCODE_GITLAB_HOST") or sys.exit("set KNOWCODE_GITLAB_HOST to the GitLab host to clone from")
    out = subprocess.run(
        ["glab", "api", "--hostname", host, "--paginate", "projects?archived=false&per_page=100&simple=true"],
        check=True, capture_output=True, text=True,
    ).stdout
    projects = [p for p in json.loads(out.replace("][", ",")) if p["namespace"]["kind"] == "group" and not p.get("empty_repo")]
    # Directory names are the project path; paths shared by several projects take their namespace too.
    counts = {}
    for p in projects:
        counts[p["path"].lower()] = counts.get(p["path"].lower(), 0) + 1
    for p in projects:
        p["dir"] = p["path"] if counts[p["path"].lower()] == 1 else p["path_with_namespace"].replace("/", ".")
    return sorted(projects, key=lambda p: p["dir"].lower())


def gitlab_one(p):
    d = GITLAB / p["dir"]
    env = {**os.environ, "GIT_LFS_SKIP_SMUDGE": "1", "GIT_TERMINAL_PROMPT": "0"}
    since = f"--shallow-since={HISTORY_DAYS} days ago"
    try:
        if (d / ".git").exists() and head(d) is None:
            subprocess.run(["rm", "-rf", str(d)], check=True)  # a clone that never completed
        if (d / ".git").exists():
            r = subprocess.run(["git", "-C", str(d), "fetch", "-q", since, "origin", p["default_branch"]], capture_output=True, env=env)
            if r.returncode != 0:
                # No commit in the window (the server hangs up on --shallow-since): the clone we have is current.
                return p, head(d), "kept"
            subprocess.run(["git", "-C", str(d), "checkout", "-q", "--detach", "FETCH_HEAD"], check=True, capture_output=True, env=env)
            verb = "updated"
        else:
            # --shallow-since fails on a repo with no commit in the window; fall back to the last commit.
            r = subprocess.run(["git", "clone", "-q", "--single-branch", "--no-tags", since, p["ssh_url_to_repo"], str(d)], capture_output=True, env=env)
            if r.returncode != 0:
                subprocess.run(["rm", "-rf", str(d)], check=True)
                subprocess.run(["git", "clone", "-q", "--single-branch", "--no-tags", "--depth", "1", p["ssh_url_to_repo"], str(d)], check=True, capture_output=True, env=env)
            verb = "cloned"
        return p, head(d), verb
    except subprocess.CalledProcessError as e:
        return p, None, (e.stderr or b"").decode(errors="replace").strip().splitlines()[-1:] or ["failed"]


def gitlab(jobs=8):
    projects = gitlab_projects()
    GITLAB.mkdir(parents=True, exist_ok=True)
    manifest = {}
    with ThreadPoolExecutor(jobs) as pool:
        for p, sha, verb in pool.map(gitlab_one, projects):
            if sha:
                manifest[p["dir"]] = {"project": p["path_with_namespace"], "sha": sha}
                print(f"{verb:<8}{p['dir']} {sha[:12]}")
            elif isinstance(verb, str):
                print(f"empty   {p['dir']} (no commits)")
            else:
                print(f"FAILED  {p['dir']}: {verb[0]}")
    (GITLAB / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"{len(manifest)} of {len(projects)} projects in {GITLAB}")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "fetch":
        fetch()
    elif cmd == "pins":
        pins()
    elif cmd == "gitlab":
        gitlab(int(sys.argv[3]) if len(sys.argv) > 3 and sys.argv[2] == "--jobs" else 8)
    elif cmd == "path" and len(sys.argv) > 2:
        print(path(sys.argv[2]))
    else:
        sys.exit(__doc__)
