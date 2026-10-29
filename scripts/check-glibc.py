#!/usr/bin/env python3
"""Fail if a Linux binary needs a glibc newer than the oldest one KnowCode supports.

Usage: python3 scripts/check-glibc.py [--max 2.28] PATH...
Each PATH is a binary, a release archive (.tar.gz, .tgz, .tar.xz), or a directory whose Linux archives are checked.
It reads each ELF file's version-needed table (.gnu.version_r), the list the dynamic loader checks at startup, so it
reports what `objdump -T` would show without needing binutils. At least one ELF file must be checked, so a moved
artifact can't make the check pass vacuously.
"""
import argparse
from pathlib import Path
import struct
import sys
import tarfile

# RHEL 8 / Rocky 8, Debian 10, Ubuntu 20.04 (2.31). The release builds link against 2.28 (dist-workspace.toml).
OLDEST = "2.28"
ARCHIVES = (".tar.gz", ".tgz", ".tar.xz")
SHT_GNU_VERNEED = 0x6FFFFFFE


def needed_versions(data: bytes) -> dict[str, set[str]]:
    """Library -> version names it must provide, from an ELF file's version-needed table."""
    if data[:4] != b"\x7fELF":
        raise ValueError("not an ELF file")
    wide = data[4] == 2
    order = "<" if data[5] == 1 else ">"
    if wide:
        shoff, = struct.unpack_from(order + "Q", data, 0x28)
        shentsize, shnum = struct.unpack_from(order + "HH", data, 0x3A)
    else:
        shoff, = struct.unpack_from(order + "I", data, 0x20)
        shentsize, shnum = struct.unpack_from(order + "HH", data, 0x2E)

    def section(index: int) -> tuple[int, int, int, int]:
        """(type, offset, link, info) of a section header."""
        at = shoff + index * shentsize
        if wide:
            _, kind, _, _, offset, _, link, info = struct.unpack_from(order + "IIQQQQII", data, at)
        else:
            _, kind, _, _, offset, _, link, info = struct.unpack_from(order + "IIIIIIII", data, at)
        return kind, offset, link, info

    def string(table: int, at: int) -> str:
        start = table + at
        return data[start:data.index(b"\0", start)].decode()

    found: dict[str, set[str]] = {}
    for index in range(shnum):
        kind, offset, link, count = section(index)
        if kind != SHT_GNU_VERNEED:
            continue
        strings = section(link)[1]
        entry = offset
        for _ in range(count):
            _, aux_count, file_name, aux, following = struct.unpack_from(order + "HHIII", data, entry)
            versions = found.setdefault(string(strings, file_name), set())
            at = entry + aux
            for _ in range(aux_count):
                _, _, _, name, aux_next = struct.unpack_from(order + "IHHII", data, at)
                versions.add(string(strings, name))
                at += aux_next
            entry += following
    return found


def version_key(name: str) -> tuple[int, ...]:
    return tuple(int(part) for part in name.split("_", 1)[1].split("."))


def files(arg: str):
    path = Path(arg)
    if path.is_dir():
        for archive in sorted(p for p in path.iterdir() if p.is_file() and "linux" in p.name and p.name.endswith(ARCHIVES)):
            yield from files(str(archive))
    elif path.name.endswith(ARCHIVES):
        with tarfile.open(path) as archive:
            for info in archive:
                if info.isfile():
                    data = archive.extractfile(info).read()
                    if data[:4] == b"\x7fELF":
                        yield f"{path}!{info.name}", data
    else:
        yield str(path), path.read_bytes()


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--max", default=OLDEST, help=f"newest glibc version allowed (default {OLDEST})")
    parser.add_argument("paths", nargs="+")
    args = parser.parse_args(argv)
    limit = tuple(int(part) for part in args.max.split("."))
    checked = 0
    failures = 0
    for arg in args.paths:
        for label, data in files(arg):
            checked += 1
            glibc = sorted({v for versions in needed_versions(data).values() for v in versions if v.startswith("GLIBC_2")},
                           key=version_key)
            newest = glibc[-1] if glibc else "none"
            over = [v for v in glibc if version_key(v) > limit]
            if over:
                failures += 1
                print(f"check-glibc: {label} needs {', '.join(over)}; the limit is GLIBC_{args.max}", file=sys.stderr)
            else:
                print(f"check-glibc: {label} needs at most {newest} (limit GLIBC_{args.max})")
    if not checked:
        print(f"check-glibc: no ELF files to check in {' '.join(args.paths)}", file=sys.stderr)
        return 1
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
