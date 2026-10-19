#!/usr/bin/env python3
"""Regenerate cargo-dist's workflow with a mandatory successful-test gate on publishing.

Usage: python3 scripts/generate-release.py [--dist /path/to/dist] [--check]
Run from any directory. Generation happens in an isolated copy; --check never edits the workspace.
"""
import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def gate(text: str) -> str:
    # Every native release build must use the same source-path remapping as local packaging.
    build = "          dist build "
    wrapped = "          bash scripts/build-release dist "
    assert text.count(build) + text.count(wrapped) == 2, "cargo-dist build layout changed; review source-path remapping"
    text = text.replace(build, wrapped)
    anchor = "  host:\n    needs:\n      - plan\n"
    assert text.count(anchor) == 1, "cargo-dist host layout changed; review the publishing gate"
    if anchor + "      - custom-ci\n" not in text:
        text = text.replace(anchor, anchor + "      - custom-ci\n", 1)
    old = "if: ${{ always() && needs.plan.result == 'success' && needs.plan.outputs.publishing == 'true' && (needs.build-global-artifacts.result == 'skipped' || needs.build-global-artifacts.result == 'success') && (needs.build-local-artifacts.result == 'skipped' || needs.build-local-artifacts.result == 'success') }}"
    new = "if: ${{ always() && needs.plan.result == 'success' && needs.custom-ci.result == 'success' && needs.plan.outputs.publishing == 'true' && needs.build-global-artifacts.result == 'success' && needs.build-local-artifacts.result == 'success' }}"
    if new not in text:
        assert text.count(old) == 1, "cargo-dist publishing condition changed; review it"
        text = text.replace(old, new, 1)
    text = harden(text)
    note = "# Regenerate with scripts/generate-release.py; it adds the required successful-test gate on host.\n"
    if not text.startswith(note):
        text = note + text
    return text


def replace(text: str, old: str, new: str, what: str, count: int = 1) -> str:
    """Replace `old` (expected `count` times) with `new`; a text already carrying `new` and no `old` is left as is."""
    if text.count(new) >= count:
        return text
    assert text.count(old) == count, f"cargo-dist {what} layout changed; review it"
    return text.replace(old, new)


TAP_AUTHOR = "KnowCode release bot"
TAP_EMAIL = "knowcode-release-bot@users.noreply.github.com"


def harden(text: str) -> str:
    """Least privilege, a checksum-pinned generator, our own tap commit identity, and no tag text in shell source."""
    # Read-only by default; only the job creating the GitHub Release writes contents (attestations are on the build
    # job already). The tap is pushed with its own token, not this workflow's.
    text = replace(text, 'permissions:\n  "contents": "write"\n', 'permissions:\n  "contents": "read"\n', "workflow permissions")
    host = "  host:\n    needs:\n"
    text = replace(text, '    runs-on: "ubuntu-24.04"\n    outputs:\n      val: ${{ steps.host.outputs.manifest }}\n',
                   '    runs-on: "ubuntu-24.04"\n    permissions:\n      "contents": "write"\n    outputs:\n      val: ${{ steps.host.outputs.manifest }}\n',
                   "host permissions")
    assert text.count(host) == 1, "cargo-dist host layout changed; review its permissions"
    # The generator installer runs only when it matches the SHA-256 pinned in scripts/install-dist.sh.
    text = replace(text,
                   "      - name: Install dist\n"
                   "        # we specify bash to get pipefail; it guards against the `curl` command\n"
                   "        # failing. otherwise `sh` won't catch that `curl` returned non-0\n"
                   "        shell: bash\n"
                   "        run: \"curl --proto '=https' --tlsv1.2 -LsSf https://github.com/axodotdev/cargo-dist/releases/download/v0.33.0/cargo-dist-installer.sh | sh\"\n",
                   "      - name: Install dist\n"
                   "        # Downloads the installer and runs it only if its SHA-256 matches the pinned one.\n"
                   "        shell: bash\n"
                   "        run: bash scripts/install-dist.sh\n",
                   "plan dist install")
    text = replace(text, "      - name: Install dist\n        run: ${{ matrix.install_dist.run }}\n",
                   "      - name: Install dist\n        shell: bash\n        run: bash scripts/install-dist.sh\n", "build dist install")
    text = replace(text, '      GITHUB_USER: "axo bot"\n      GITHUB_EMAIL: "admin+bot@axo.dev"\n',
                   f'      GITHUB_USER: "{TAP_AUTHOR}"\n      GITHUB_EMAIL: "{TAP_EMAIL}"\n', "tap commit identity")
    # The tag reaches shell steps through the environment, never spliced into the script text.
    text = replace(text,
                   "      - id: plan\n        run: |\n"
                   "          dist ${{ (!github.event.pull_request && format('host --steps=create --tag={0}', github.ref_name)) || 'plan' }} --output-format=json > plan-dist-manifest.json\n",
                   "      - id: plan\n        env:\n          PUBLISH_TAG: ${{ !github.event.pull_request && github.ref_name || '' }}\n        run: |\n"
                   "          if [ -n \"$PUBLISH_TAG\" ]; then\n"
                   "            dist host --steps=create --tag=\"$PUBLISH_TAG\" --output-format=json > plan-dist-manifest.json\n"
                   "          else\n"
                   "            dist plan --output-format=json > plan-dist-manifest.json\n"
                   "          fi\n",
                   "plan step")
    flag_env = "        env:\n          TAG_FLAG: ${{ needs.plan.outputs.tag-flag }}\n"
    text = replace(text, "      - name: Build artifacts\n        run: |\n",
                   "      - name: Build artifacts\n" + flag_env + "        run: |\n", "local build step")
    text = replace(text, "      - id: cargo-dist\n        shell: bash\n        run: |\n          bash scripts/build-release dist ",
                   "      - id: cargo-dist\n        shell: bash\n" + flag_env + "        run: |\n          bash scripts/build-release dist ",
                   "global build step")
    text = replace(text, "      - id: host\n        shell: bash\n        run: |\n",
                   "      - id: host\n        shell: bash\n" + flag_env + "        run: |\n", "host step")
    text = replace(text, "${{ needs.plan.outputs.tag-flag }} ", "${TAG_FLAG:+\"$TAG_FLAG\"} ", "tag flag", count=3)
    text = replace(text, '          RELEASE_COMMIT: "${{ github.sha }}"\n',
                   '          RELEASE_COMMIT: "${{ github.sha }}"\n          RELEASE_TAG: "${{ needs.plan.outputs.tag }}"\n', "release env")
    text = replace(text, 'gh release create "${{ needs.plan.outputs.tag }}" ', 'gh release create "$RELEASE_TAG" ', "release tag")
    return text


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", default=shutil.which("dist"))
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if not args.dist:
        parser.error("install the cargo-dist version pinned in dist-workspace.toml, or pass --dist")
    root = Path(__file__).resolve().parent.parent
    workflow = root / ".github/workflows/release.yml"
    before = workflow.read_bytes() if workflow.exists() else None
    settings = (root / "dist-workspace.toml").read_bytes()
    marker = b'allow-dirty = ["ci"]\n'
    assert settings.count(marker) == 1, "expected the documented custom-CI setting"
    generating = settings.replace(marker, b"", 1)
    with tempfile.TemporaryDirectory(prefix="knowcode-dist-generate-") as temp:
        work = Path(temp)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md", "LICENSE-MIT", "LICENSE-APACHE"):
            shutil.copy2(root / name, work / name)
        for name in ("crates", ".github"):
            shutil.copytree(root / name, work / name, symlinks=True)
        # dist refuses explicit generation with allow-dirty CI. Only the temporary copy omits that setting;
        # builds in the real workspace always keep the reviewed publishing gate enabled.
        (work / "dist-workspace.toml").write_bytes(generating)
        subprocess.run([args.dist, "generate", "--mode", "ci"], cwd=work, check=True)
        generated = gate((work / ".github/workflows/release.yml").read_text()).encode()
    if args.check:
        if generated != before:
            print("release.yml differs; run scripts/generate-release.py", file=sys.stderr)
            return 1
    else:
        current = workflow.read_bytes() if workflow.exists() else None
        if current != before:
            raise RuntimeError("release.yml changed during generation; retained the concurrent edit")
        workflow.write_bytes(generated)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
