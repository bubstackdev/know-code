#!/usr/bin/env bash
# Install the release generator pinned by this workspace (for CI and release development).
# The installer is downloaded to a file and run only when it matches the SHA-256 pinned here for the version
# dist-workspace.toml names; it in turn verifies the archive it downloads against the checksums it embeds.
set -euo pipefail
cd "$(dirname "$0")/.."
dist_version=$(sed -n 's/^cargo-dist-version = "\([^"]*\)"$/\1/p' dist-workspace.toml)
case "$dist_version" in
    ""|*[!0-9.]*) echo "invalid pinned cargo-dist version" >&2; exit 1 ;;
esac
# A new cargo-dist version needs its installer's digest here: the `digest` of cargo-dist-installer.sh in
# `gh api repos/axodotdev/cargo-dist/releases/tags/v<version>`, checked against a download of the file.
case "$dist_version" in
    0.33.0) expected=c5b53ac2559eb26e3b3dc28755eb5d011ed6ec9ac97fa4fc2a2459872e766b5e ;;
    *) echo "no pinned installer checksum for cargo-dist $dist_version; add one to scripts/install-dist.sh" >&2; exit 1 ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
installer="$work/cargo-dist-installer.sh"
curl --proto '=https' --tlsv1.2 -LsSf -o "$installer" "https://github.com/axodotdev/cargo-dist/releases/download/v${dist_version}/cargo-dist-installer.sh"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$installer" | cut -d' ' -f1)
else
    actual=$(shasum -a 256 "$installer" | cut -d' ' -f1)
fi
if [ "$actual" != "$expected" ]; then
    echo "cargo-dist $dist_version installer checksum mismatch: expected $expected, got $actual" >&2
    exit 1
fi
sh "$installer"
