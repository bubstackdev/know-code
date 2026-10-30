#!/usr/bin/env python3
"""Two KnowCode versions sharing one cache both keep answering, and neither changes the other's data.

    check-cache-compat.py OTHER THIS

OTHER is another knowcode binary (CI passes the latest release), THIS the build under test. They take turns on one
cache, over a workspace of two fixture repositories, with an edit between rounds: every run must succeed and answer a
symbol lookup, a references query and a workspace-wide lookup made from outside any repository. After each run, the
other build's data must be byte for byte as it was. This build keeps its data in its own tree, `<cache>/v/<key>/`
(crates/kc-index/src/lib.rs `tree_dir`); 0.6.1 and older keep theirs at the cache's top level. The only files a run may
add outside its own data are the ones every version shares: the maintenance lock and the cursor key.

Exits 1 with the first failure.
"""

import hashlib
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kc_env import knowcode_env  # noqa: E402

SHARED = {"locks/maintenance.lock", "cursor.key"}


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def fixture(ws: Path) -> tuple[Path, Path]:
    shop, billing = ws / "shop", ws / "billing"
    for repo, files in [
        (shop, {"src/cart.ts": "export function addItem(sku: string): number {\n    return sku.length;\n}\n",
                "src/checkout.ts": "import { addItem } from './cart';\n\nexport function checkout(): number {\n    return addItem('a');\n}\n"}),
        (billing, {"src/charge.ts": "export function chargeCard(amount: number): boolean {\n    return amount > 0;\n}\n"}),
    ]:
        for path, text in files.items():
            (repo / path).parent.mkdir(parents=True, exist_ok=True)
            (repo / path).write_text(text)
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        git(repo, "-c", "user.name=Check", "-c", "user.email=check@example.test", "add", "-A")
        git(repo, "-c", "user.name=Check", "-c", "user.email=check@example.test", "commit", "-qm", "fixture")
    return shop, billing


def snapshot(cache: Path, within) -> dict[str, str]:
    """Each file under `cache` that `within` keeps, by its path relative to `cache`, with its content's hash."""
    out = {}
    for path in sorted(p for p in cache.rglob("*") if p.is_file()):
        rel = path.relative_to(cache).as_posix()
        if within(rel):
            out[rel] = hashlib.sha256(path.read_bytes()).hexdigest()
    return out


def ask(binary: Path, env: dict, cwd: Path, args: list[str], expect: str, who: str) -> None:
    run = subprocess.run([str(binary), *args], cwd=cwd, env=env, capture_output=True, text=True, timeout=600)
    if run.returncode != 0 or expect not in run.stdout:
        raise SystemExit(f"{who}: `knowcode {' '.join(args)}` in {cwd.name}: exit {run.returncode}, "
                         f"expected {expect!r}\nstdout: {run.stdout[-800:]}\nstderr: {run.stderr[-800:]}")


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    other, this = (Path(a).resolve() for a in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="knowcode-cache-compat-") as temp:
        base = Path(temp).resolve()
        env = knowcode_env(base)
        cache = Path(env["KNOWCODE_CACHE_DIR"])
        ws = base / "ws"
        shop, billing = fixture(ws)
        (Path(env["KNOWCODE_CONFIG_DIR"]) / "config.toml").write_text(
            f'[workspace]\nroots = ["{ws}"]\nmax_depth = 2\n\n[cache]\nmax_size = "10 GiB"\n')
        trees_before = set()
        mine: str | None = None  # this build's tree, `v/<key>`, once it has made one
        rounds = [("other", other), ("this", this), ("other", other), ("this", this), ("edit", None),
                  ("other", other), ("this", this), ("other", other), ("this", this)]
        for n, (who, binary) in enumerate(rounds, 1):
            if who == "edit":
                cart = shop / "src/cart.ts"
                cart.write_text(cart.read_text() + "\nexport function removeItem(sku: string): number {\n    return -sku.length;\n}\n")
                continue
            # What the other build owns, as this run starts: for this build, everything but its own tree and the
            # shared files; for the other, this build's tree.
            if who == "this":
                theirs = lambda rel: not (mine and rel.startswith(mine + "/")) and rel not in SHARED
            else:
                theirs = lambda rel: bool(mine) and rel.startswith(mine + "/")
            before = snapshot(cache, theirs)
            trees_before = {p.name for p in (cache / "v").iterdir()} if (cache / "v").is_dir() else set()
            label = f"round {n} ({who}, {binary.name})"
            ask(binary, env, shop, ["find", "addItem"], "src/cart.ts", label)
            ask(binary, env, shop, ["refs", "addItem"], "src/checkout.ts", label)
            ask(binary, env, ws, ["find", "chargeCard"], "src/charge.ts", label)
            if n > 5:
                ask(binary, env, shop, ["find", "removeItem"], "src/cart.ts", label)
            if who == "this" and mine is None:
                made = ({p.name for p in (cache / "v").iterdir()} if (cache / "v").is_dir() else set()) - trees_before
                if len(made) == 1:
                    mine = "v/" + made.pop()
                    # Files this run made in its new tree aren't the other build's.
                    before = {k: v for k, v in before.items() if not k.startswith(mine + "/")}
            after = snapshot(cache, theirs)
            changed = sorted(k for k in before if after.get(k) != before[k])
            if changed:
                raise SystemExit(f"{label} changed the other build's files: {changed[:10]}")
            print(f"{label}: answered; the other build's {len(before)} files unchanged")
        if mine is None:
            raise SystemExit("this build made no tree of its own under v/; is it a build with per-version cache trees?")
    print("cache compatibility: both builds answered every round, and neither changed the other's data")


if __name__ == "__main__":
    main()
