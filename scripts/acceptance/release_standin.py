#!/usr/bin/env python3
"""A stand-in for GitHub's release endpoints, so the update from a published release can be checked with no network.

    release_standin.py prepare --release TAG=DIR [--release TAG=DIR ...] --latest TAG --out SHIM
    release_standin.py requests SHIM

KnowCode's release check and self-update run `curl` from PATH (crates/knowcode/src/update.rs), and so does the
installer a release ships (`curl -sSfL URL -o FILE`). Neither has an address that can be pointed elsewhere, and none
is added for this: `prepare` writes SHIM/curl, an executable that is put first on PATH in place of the real curl and
answers exactly the public URLs of bubstackdev/know-code's releases from local files:

- `https://api.github.com/repos/bubstackdev/know-code/releases/latest`: the latest release, as GitHub's API gives it
  (`tag_name`, `draft`, `prerelease`, and per asset its `name`, `size`, `browser_download_url` and `sha256:` digest),
  built from the files in its DIR;
- `https://github.com/bubstackdev/know-code/releases/latest`: a 302 to `…/releases/tag/<latest>`;
- `https://github.com/bubstackdev/know-code/releases/download/<TAG>/<NAME>`: the file NAME in that release's DIR.

Every other URL is refused as curl refuses an HTTP error (exit 22), and every request, answered or refused, is appended
to SHIM/requests.jsonl. `requests` prints the URLs asked, one per line, and exits 1 if any was refused, so a run can
show that no other address was asked for. Each DIR holds one release's files as GitHub serves them (the installer,
the archives and their checksums). The shim needs only Python 3 and the standard library, so it runs in a container.
"""
import argparse
import hashlib
import json
import sys
from pathlib import Path

REPO = "bubstackdev/know-code"
API = f"https://api.github.com/repos/{REPO}/releases/latest"
PAGES = f"https://github.com/{REPO}/releases"

# The shim itself. It reads its releases from standin.json beside it, so the file can be copied into a container
# whole. It answers both of curl's spellings that its callers use (`--output F` and `-o F`, `--dump-header F`), writes
# the body to stdout when no output file is named, and leaves every other flag alone.
SHIM = r'''#!/usr/bin/env python3
import json, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
conf = json.loads((HERE / "standin.json").read_text())
args = sys.argv[1:]
url = next((a for a in args if a.startswith(("https://", "http://"))), None)

def value(*names):
    for i, a in enumerate(args[:-1]):
        if a in names:
            return args[i + 1]
    return None

def log(status):
    with (HERE / "requests.jsonl").open("a") as f:
        f.write(json.dumps({"url": url, "status": status, "args": args}) + "\n")

def refuse(status, why):
    log(status)
    sys.stderr.write(f"curl: (22) The requested URL returned error: {status} ({why})\n")
    raise SystemExit(22)

def answer(body, status=200, headers=()):
    dump = value("--dump-header", "-D")
    if dump:
        lines = [f"HTTP/2 {status} "] + [f"{k}: {v}" for k, v in headers] + [f"content-length: {len(body)}", "", ""]
        Path(dump).write_text("\r\n".join(lines))
    out = value("--output", "-o")
    if out:
        Path(out).write_bytes(body)
    else:
        sys.stdout.buffer.write(body)
    log(status)

if url is None:
    refuse(400, "no URL among the arguments")
api, pages, latest = conf["api"], conf["pages"], conf["latest"]
if url == api:
    answer(json.dumps(conf["releases"][latest]["json"]).encode())
elif url == pages + "/latest":
    answer(b"", 302, [("location", f"{pages}/tag/{latest}")])
elif url.startswith(pages + "/download/"):
    tag, _, name = url[len(pages) + len("/download/"):].partition("/")
    release = conf["releases"].get(tag)
    if release is None or name not in release["files"]:
        refuse(404, "not a file of a release this stand-in serves")
    answer(Path(release["dir"], name).read_bytes())
else:
    refuse(404, "not a release URL of " + conf["repo"])
'''


def release_json(tag, directory):
    """The API's view of one release: what self-update reads (tag, flags and each asset's URL and digest)."""
    assets = []
    for f in sorted(Path(directory).iterdir()):
        if f.is_file():
            data = f.read_bytes()
            assets.append({"name": f.name, "size": len(data), "browser_download_url": f"{PAGES}/download/{tag}/{f.name}",
                           "digest": "sha256:" + hashlib.sha256(data).hexdigest()})
    return {"tag_name": tag, "name": tag, "draft": False, "prerelease": False, "assets": assets,
            "html_url": f"{PAGES}/tag/{tag}"}


def prepare(releases, latest, out):
    if latest not in releases:
        raise SystemExit(f"--latest {latest} is not among the releases given")
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    conf = {"repo": REPO, "api": API, "pages": PAGES, "latest": latest, "releases": {}}
    for tag, directory in releases.items():
        directory = Path(directory).resolve()
        if not directory.is_dir():
            raise SystemExit(f"{directory} is not a directory")
        conf["releases"][tag] = {"dir": str(directory), "files": sorted(f.name for f in directory.iterdir() if f.is_file()),
                                 "json": release_json(tag, directory)}
    (out / "standin.json").write_text(json.dumps(conf, indent=1))
    shim = out / "curl"
    shim.write_text(SHIM)
    shim.chmod(0o755)
    (out / "requests.jsonl").unlink(missing_ok=True)
    return shim


def requests(shim_dir):
    """The requests the shim saw, and whether any was refused."""
    path = Path(shim_dir) / "requests.jsonl"
    rows = [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
    return rows, any(r["status"] >= 400 for r in rows)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--release", action="append", required=True, metavar="TAG=DIR")
    p.add_argument("--latest", required=True)
    p.add_argument("--out", required=True)
    r = sub.add_parser("requests")
    r.add_argument("shim")
    a = ap.parse_args(argv)
    if a.cmd == "prepare":
        releases = {}
        for item in a.release:
            tag, sep, directory = item.partition("=")
            if not sep or not tag or not directory:
                raise SystemExit(f"--release wants TAG=DIR, not {item!r}")
            releases[tag] = directory
        print(prepare(releases, a.latest, a.out))
        return 0
    rows, refused = requests(a.shim)
    for row in rows:
        print(row["status"], row["url"])
    return 1 if refused else 0


if __name__ == "__main__":
    sys.exit(main())
