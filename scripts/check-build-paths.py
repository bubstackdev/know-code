#!/usr/bin/env python3
"""Fail if release artifacts carry the builder's home, Cargo home, or checkout path.

Usage: python3 scripts/check-build-paths.py PATH...
Each PATH is a release archive (.tar.gz, .tar.xz, .zip), a binary, or a directory whose archives are checked.
Every regular file inside an archive is searched for the raw bytes of each prefix, which covers what `strings`
would show. At least one file must be checked, so a moved artifact cannot make the check pass vacuously.
"""
import os
from pathlib import Path
import sys
import tarfile
import zipfile

ARCHIVES = (".tar.gz", ".tgz", ".tar.xz", ".zip")


def prefixes() -> list[str]:
    home = os.environ.get("HOME", "")
    candidates = [
        home,
        os.environ.get("CARGO_HOME") or (os.path.join(home, ".cargo") if home else ""),
        os.environ.get("GITHUB_WORKSPACE", ""),
        str(Path(__file__).resolve().parent.parent),
    ]
    found = []
    for candidate in candidates:
        for form in (candidate.rstrip("/"), os.path.realpath(candidate) if candidate else ""):
            # A bare "/" or a one-component prefix would match unrelated strings.
            if form.count("/") >= 2 or (form.count("/") == 1 and len(form) > 5):
                if form not in found:
                    found.append(form)
    return found


def members(path: Path):
    name = path.name
    if name.endswith(".zip"):
        with zipfile.ZipFile(path) as archive:
            for info in archive.infolist():
                if not info.is_dir():
                    yield f"{path}!{info.filename}", archive.read(info)
    elif name.endswith(ARCHIVES):
        with tarfile.open(path) as archive:
            for info in archive:
                if info.isfile():
                    yield f"{path}!{info.name}", archive.extractfile(info).read()
    else:
        yield str(path), path.read_bytes()


def context(data: bytes, needle: bytes) -> str:
    at = data.find(needle)
    start = max(data.rfind(b"\0", 0, at) + 1, at - 40)
    end = data.find(b"\0", at)
    end = min(end if end != -1 else len(data), at + len(needle) + 80)
    return data[start:end].decode("utf-8", "replace")


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    paths = []
    for arg in argv:
        path = Path(arg)
        if path.is_dir():
            paths.extend(sorted(p for p in path.iterdir() if p.is_file() and p.name.endswith(ARCHIVES)))
        else:
            paths.append(path)
    needles = [(prefix, prefix.encode()) for prefix in prefixes()]
    checked = 0
    failures = []
    for path in paths:
        for label, data in members(path):
            checked += 1
            for prefix, needle in needles:
                if needle in data:
                    failures.append(f"{label}: contains {prefix!r}, e.g. {context(data, needle)!r}")
    if not checked:
        print(f"check-build-paths: no files to check in {' '.join(argv)}", file=sys.stderr)
        return 1
    for failure in failures:
        print(f"check-build-paths: {failure}", file=sys.stderr)
    if failures:
        return 1
    print(f"check-build-paths: {checked} files carry none of {len(needles)} build prefixes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
