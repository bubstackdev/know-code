#!/usr/bin/env bash
# Install the cargo-deny release CI's deny job runs (Linux x86_64 and arm64), into ${CARGO_HOME:-~/.cargo}/bin.
# The repository's Actions policy allows only GitHub-owned actions, so cargo-deny's own action isn't used: the release
# archive is downloaded and unpacked only when it matches the SHA-256 pinned here, which is the digest GitHub records
# for that asset (`gh api repos/EmbarkStudios/cargo-deny/releases/tags/<version>`), checked against a download.
set -euo pipefail
version=0.20.2
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-musl expected=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f ;;
    Linux-aarch64) target=aarch64-unknown-linux-musl expected=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d ;;
    *) echo "no pinned cargo-deny $version build for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
name="cargo-deny-$version-$target"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --proto '=https' --tlsv1.2 -LsSf -o "$work/$name.tar.gz" \
    "https://github.com/EmbarkStudios/cargo-deny/releases/download/$version/$name.tar.gz"
actual=$(sha256sum "$work/$name.tar.gz" | cut -d' ' -f1)
if [ "$actual" != "$expected" ]; then
    echo "cargo-deny $version checksum mismatch: expected $expected, got $actual" >&2
    exit 1
fi
tar xzf "$work/$name.tar.gz" -C "$work"
bin="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$bin"
install -m 0755 "$work/$name/cargo-deny" "$bin/cargo-deny"
"$bin/cargo-deny" --version
