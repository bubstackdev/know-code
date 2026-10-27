#!/usr/bin/env python3
"""Fetch pinned public source into a new external directory and export separate immutable source views.

No project builds, dependency installs, submodule initialization, model calls, or KnowCode queries occur.
Git export attributes apply and omissions are recorded. Existing directories are never refreshed or replaced.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "eval/public-v1/manifest.json"


def safe_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts or "\\" in value or ".git" in path.parts:
        raise ValueError("unsafe source path: " + value)
    return path


def validate(manifest: dict) -> list[dict]:
    if manifest.get("format") != 1 or not manifest.get("families"):
        raise ValueError("unsupported or empty manifest")
    repos, names = [], set()
    for family in manifest["families"]:
        for repo in family["repos"]:
            name = repo["name"]
            if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name) or name in names:
                raise ValueError("invalid or duplicate repository name")
            names.add(name)
            if not re.fullmatch(r"https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\.git", repo["url"]):
                raise ValueError("expected a public GitHub HTTPS origin")
            if not re.fullmatch(r"[0-9a-f]{40}", repo["commit"]):
                raise ValueError("repository commit must be a full pinned SHA")
            for path in repo["exclude"] + [repo["license_notice"]]:
                safe_path(path)
            repos.append(dict(repo, family=family["name"]))
    return repos


def git(repo: Path, *args: str) -> bytes:
    child = subprocess.Popen(["git", "-c", "core.hooksPath=/dev/null", "-C", str(repo), *args], stdin=subprocess.DEVNULL,
                             env=os.environ | {"GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0", "GIT_LFS_SKIP_SMUDGE": "1"},
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    try:
        stdout, stderr = child.communicate(timeout=180)
    except subprocess.TimeoutExpired:
        # This function created the session/group; only this Git command's own children are signalled.
        os.killpg(child.pid, signal.SIGKILL)
        child.communicate(timeout=5)
        raise
    if child.returncode:
        raise RuntimeError(stderr.decode(errors="replace"))
    return stdout


# A privacy file (KnowCode's, or a code base's own under its own name): any dotfile naming privacy, in YAML or TOML.
PRIVACY_FILE = re.compile(r"^\.[\w.-]*privacy[\w.-]*\.(?:ya?ml|toml)$", re.I)


def export(repo: Path, commit: str, destination: Path, exclusions: list[str]) -> dict:
    """Copy archive bytes, never hardlink or execute source. Reject special files and escaping symlinks."""
    if destination.exists():
        raise ValueError("view already exists; use a new output directory")
    tracked = [os.fsdecode(name) for name in git(repo, "ls-tree", "-r", "--name-only", "-z", commit).split(b"\0") if name]
    if any(PRIVACY_FILE.match(PurePosixPath(name).name) for name in tracked):
        raise ValueError("source has privacy metadata; resolve its permitted scope before exporting a view")
    excluded = [str(safe_path(item)).rstrip("/") for item in exclusions]
    def omit(name: str) -> bool:
        return any(name == item or name.startswith(item + "/") for item in excluded)
    destination.mkdir(parents=True)
    files, links = [], []
    with tempfile.TemporaryDirectory(prefix="source-archive-", dir=destination.parent) as temp:
        archive = Path(temp) / "source.tar"
        git(repo, "archive", "--format=tar", "--output=" + str(archive), commit)
        with tarfile.open(archive, mode="r|") as stream:
            for member in stream:
                path = safe_path(member.name)
                name = path.as_posix()
                if omit(name):
                    continue
                target = destination / name
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                if member.isfile():
                    source = stream.extractfile(member)
                    if source is None:
                        raise ValueError("archive file has no payload")
                    with source:
                        data = source.read()
                    with target.open("xb") as output:
                        output.write(data)
                    target.chmod(0o555 if member.mode & 0o111 else 0o444)
                    link = False
                elif member.issym():
                    if PurePosixPath(member.linkname).is_absolute() or "\\" in member.linkname:
                        raise ValueError("absolute or platform-dependent source symlink")
                    data = os.fsencode(member.linkname)
                    target.symlink_to(member.linkname)
                    links.append(target)
                    link = True
                else:
                    raise ValueError("unsupported archive entry: " + name)
                files.append({"path": name, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data), "symlink": link})
        for link in links:
            if not link.resolve().is_relative_to(destination.resolve()) or not link.exists():
                raise ValueError("source symlink escapes view or targets an excluded/missing file")
        for directory, children, _ in os.walk(destination, topdown=False, followlinks=False):
            Path(directory).chmod(0o555)  # Only directories created for this new view.
        names = {item["path"] for item in files}
        return {"files": sorted(files, key=lambda item: item["path"]),
                "excluded_paths": sorted(name for name in tracked if omit(name)),
                "git_export_omissions": sorted(name for name in tracked if name not in names and not omit(name))}



def prepare(manifest_path: Path, destination: Path) -> None:
    raw = manifest_path.read_bytes()
    repos = validate(json.loads(raw))
    destination = destination.resolve()
    if destination.is_relative_to(ROOT):
        raise ValueError("clones, views and receipts must stay outside the source repository")
    if destination.exists() and any(destination.iterdir()):
        raise ValueError("output directory must be new or empty; existing data is retained")
    destination.mkdir(parents=True, exist_ok=True, mode=0o700)
    receipt = {"manifest_sha256": hashlib.sha256(raw).hexdigest(), "stage": "preparing", "repos": [],
               "git_version": subprocess.check_output(["git", "--version"], text=True).strip(),
               "tool_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    report = destination / "source-receipt.json"
    def save() -> None:
        report.write_text(json.dumps(receipt, indent=2) + "\n")
    save()
    try:
        for repo in repos:
            store = destination / "objects" / repo["name"]
            store.mkdir(parents=True)
            git(store, "init", "--bare", "--template=", "--quiet")
            git(store, "remote", "add", "origin", repo["url"])
            git(store, "fetch", "--quiet", "--depth=1", "origin", repo["commit"])
            if git(store, "rev-parse", "FETCH_HEAD").decode().strip() != repo["commit"]:
                raise ValueError("fetched commit differs from manifest")
            view = destination / "view" / repo["name"]
            metadata = export(store, repo["commit"], view, repo["exclude"])
            if not (view / repo["license_notice"]).is_file():
                raise ValueError("declared license notice is absent from exported scope")
            receipt["repos"].append(dict(repo, view=str(view), **metadata))
            save()
            print(f"prepared {repo['name']} {repo['commit']}", flush=True)
        receipt["stage"] = "source-ready; questions, labels and scored runs still pending"
    except Exception as exc:
        receipt.update(stage="failed", error=repr(exc))
        raise
    finally:
        save()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument("--out", type=Path, required=True, help="New or empty directory outside the source tree")
    args = parser.parse_args()
    prepare(args.manifest.resolve(strict=True), args.out)
