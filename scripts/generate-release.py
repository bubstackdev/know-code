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
    if anchor + "      - custom-release-gate\n" not in text:
        text = text.replace(anchor, anchor + "      - custom-release-gate\n", 1)
    # Publishing needs every gate to have passed: a skipped build or portability check (linux-compat.yml) isn't enough.
    old = "if: ${{ always() && needs.plan.result == 'success' && needs.plan.outputs.publishing == 'true' && (needs.build-global-artifacts.result == 'skipped' || needs.build-global-artifacts.result == 'success') && (needs.custom-linux-compat.result == 'skipped' || needs.custom-linux-compat.result == 'success') && (needs.build-local-artifacts.result == 'skipped' || needs.build-local-artifacts.result == 'success') }}"
    new = "if: ${{ always() && needs.plan.result == 'success' && needs.custom-release-gate.result == 'success' && needs.plan.outputs.publishing == 'true' && needs.build-global-artifacts.result == 'success' && needs.custom-linux-compat.result == 'success' && needs.build-local-artifacts.result == 'success' }}"
    if new not in text:
        assert text.count(old) == 1, "cargo-dist publishing condition changed; review it"
        text = text.replace(old, new, 1)
    # The gate (release-gate.yml) reads the tagged commit's ci.yml runs: its job may read Actions, and nothing more.
    gate_job = "  custom-release-gate:\n    uses: ./.github/workflows/release-gate.yml\n"
    text = replace(text, gate_job, gate_job.replace("    uses:", '    permissions:\n      "actions": "read"\n      "contents": "read"\n    uses:'), "release gate")
    # The portability check (linux-compat.yml) reads the built archives and needs no secret and no write access.
    compat_job = ("  custom-linux-compat:\n    needs:\n      - plan\n      - build-local-artifacts\n"
                  "    uses: ./.github/workflows/linux-compat.yml\n    with:\n      plan: ${{ needs.plan.outputs.val }}\n")
    text = replace(text, compat_job + "    secrets: inherit\n",
                   compat_job.replace("    uses:", '    permissions:\n      "contents": "read"\n    uses:'), "portability check")
    text = harden(text)
    # Pull requests are gated by ci.yml alone (pr-run-mode = "skip"); the release workflow runs on tag pushes only, and
    # doesn't run CI again: release-gate.yml requires ci.yml to have passed on the tagged commit.
    assert "\n  pull_request:\n" not in text, "release.yml runs on pull requests; keep pr-run-mode = \"skip\""
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


def harden(text: str) -> str:
    """Least privilege, a checksum-pinned generator, and no tag text in shell source."""
    # Read-only by default; only the job creating the GitHub Release writes contents (attestations are on the build
    # job already).
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
    text = check_build_paths(text)
    text = homebrew(text)
    return text


HOMEBREW_TOKEN = "${{ secrets.HOMEBREW_TAP_TOKEN }}"
BOT_NAME = "KnowCode release bot"
BOT_EMAIL = "41898282+github-actions[bot]@users.noreply.github.com"


def homebrew(text: str) -> str:
    """The tap publish job: read-only GITHUB_TOKEN, KnowCode's own commit identity, and the tap token kept out of
    the checkout's git config and out of every step but the push, which receives it through `env:`."""
    job = "  publish-homebrew-formula:\n"
    assert text.count(job) == 1, "expected the Homebrew publish job; review dist-workspace.toml's publish-jobs"
    # The tap token lives in the `release` environment, which only `v*.*.*` tags may deploy to (docs/releasing.md).
    text = replace(text, '    runs-on: "ubuntu-24.04"\n    env:\n      GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n'
                   '      PLAN: ${{ needs.plan.outputs.val }}\n',
                   '    runs-on: "ubuntu-24.04"\n    environment: release\n    permissions:\n      "contents": "read"\n'
                   '    env:\n      GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n      PLAN: ${{ needs.plan.outputs.val }}\n',
                   "Homebrew job permissions and environment")
    text = replace(text, '      GITHUB_USER: "axo bot"\n      GITHUB_EMAIL: "admin+bot@axo.dev"\n',
                   f'      GITHUB_USER: "{BOT_NAME}"\n      GITHUB_EMAIL: "{BOT_EMAIL}"\n', "Homebrew commit identity")
    text = replace(text,
                   "          persist-credentials: true\n          repository: \"bubstackdev/homebrew-tap\"\n"
                   f"          token: {HOMEBREW_TOKEN}\n",
                   "          persist-credentials: false\n          repository: \"bubstackdev/homebrew-tap\"\n",
                   "Homebrew tap checkout")
    # `git config --global` sets both author and committer; the variables make that explicit for `git commit`.
    text = replace(text, '            git add "Formula/${filename}"\n            git commit -m "${name} ${version}"\n'
                   '          done\n          git push\n',
                   '            git add "Formula/${filename}"\n'
                   '            GIT_AUTHOR_NAME="${GITHUB_USER}" GIT_AUTHOR_EMAIL="${GITHUB_EMAIL}" '
                   'GIT_COMMITTER_NAME="${GITHUB_USER}" GIT_COMMITTER_EMAIL="${GITHUB_EMAIL}" '
                   'git commit -m "${name} ${version}"\n'
                   '          done\n'
                   '      - name: Push formula files\n'
                   '        env:\n'
                   f'          TAP_TOKEN: {HOMEBREW_TOKEN}\n'
                   '        run: |\n'
                   '          auth=$(printf \'x-access-token:%s\' "$TAP_TOKEN" | base64 -w0)\n'
                   '          echo "::add-mask::$auth"\n'
                   '          git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $auth" push\n',
                   "Homebrew push")
    return text


def check_build_paths(text: str) -> str:
    """Each native build fails, before attestation, if its archives carry the builder's home or checkout path."""
    built = '          echo "dist ran successfully"\n'
    step = ("      - name: Check for build-machine paths\n"
            "        shell: bash\n"
            "        run: python3 scripts/check-build-paths.py target/distrib\n")
    return replace(text, built + "      - name: Attest\n", built + step + "      - name: Attest\n", "build-path check")


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
