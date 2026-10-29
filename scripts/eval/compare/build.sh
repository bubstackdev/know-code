#!/usr/bin/env bash
# Builds compare-v1's images (eval/compare-v1/README.md, "The container"), one per language, from
# eval/compare-v1/Dockerfile and the pins in eval/compare-v1/tools.toml.
#
#     build.sh SOURCE WORK LANGUAGE [...]
#
# SOURCE is the KnowCode checkout under test; it is built for Linux in the pinned rust image, with a target directory
# of its own under WORK (named by the checkout's path, so two checkouts never share one), and only the binary goes into
# the image's build context. Images are tagged
# kc-compare-v1:LANGUAGE and stay local; WORK/images.txt records each image's id. Run with PYTHON pointing at
# Python 3.11 or later (tomllib).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
SOURCE=$(cd "${1:?usage: build.sh SOURCE WORK LANGUAGE [...]}" && pwd)
WORK=${2:?usage: build.sh SOURCE WORK LANGUAGE [...]}
shift 2
PYTHON=${PYTHON:-python3}
mkdir -p "$WORK/context"
WORK=$(cd "$WORK" && pwd)
TARGET=$WORK/target-$(printf '%s' "$SOURCE" | shasum -a 256 | cut -c1-12)
mkdir -p "$TARGET"

pin() { "$PYTHON" -c 'import sys, tomllib; d = tomllib.load(open(sys.argv[1], "rb"))
for k in sys.argv[2].split("."): d = d[k]
print(d)' "$ROOT/eval/compare-v1/tools.toml" "$1"; }

case $(uname -m) in arm64|aarch64) ARCH=arm64 UARCH=aarch64 ;; *) ARCH=amd64 UARCH=x86_64 ;; esac
# Named on every run and build: a local store may hold another platform's image under the same index digest.
PLATFORM=linux/$ARCH

# KnowCode for Linux, in the pinned rust image. The source is mounted read-only; nothing is written into it.
docker run --rm --platform "$PLATFORM" -v "$SOURCE:/src:ro" -v "$TARGET:/target" -e CARGO_TARGET_DIR=/target \
  -e CARGO_HOME=/target/cargo-home "$(pin images.rust)" \
  bash -c 'cd /src && cargo build --release --locked -p knowcode' >"$WORK/knowcode-build.log" 2>&1
cp "$TARGET/release/knowcode" "$WORK/context/knowcode"

for LANGUAGE in "$@"; do
  CBM_URL=$(pin codebase-memory-mcp.pinned.url)
  UV_URL=$(pin uv.url)
  docker build --platform "$PLATFORM" -f "$ROOT/eval/compare-v1/Dockerfile" -t "kc-compare-v1:$LANGUAGE" \
    --build-arg UV_URL="${UV_URL//\{uarch\}/$UARCH}" --build-arg UV_SHA256="$(pin "uv.sha256.$ARCH")" \
    --build-arg BASE_IMAGE="$(pin "images.$LANGUAGE")" \
    --build-arg LANGUAGE="$LANGUAGE" --build-arg ARCH="$ARCH" \
    --build-arg CBM_URL="${CBM_URL//\{arch\}/$ARCH}" \
    --build-arg CBM_SHA256="$(pin "codebase-memory-mcp.pinned.sha256.$ARCH")" \
    --build-arg SERENA_URL="$(pin serena.pinned.url)" \
    --build-arg SERENA_SHA256="$(pin serena.pinned.sha256)" \
    --build-arg SERENA_PYTHON="$(pin serena.pinned.python)" \
    "$WORK/context" >"$WORK/build-$LANGUAGE.log" 2>&1
  echo "$LANGUAGE $(docker image inspect --format '{{.Id}}' "kc-compare-v1:$LANGUAGE")" | tee -a "$WORK/images.txt"
done
