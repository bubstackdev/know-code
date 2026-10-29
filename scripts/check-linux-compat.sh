#!/usr/bin/env bash
# Run a Linux knowcode binary on the oldest distributions it supports: its version, then a tiny index and two queries
# over a throwaway repository. With check-glibc.py (which reads the symbol versions the binary needs) this is the
# release's portability gate: the binary is linked against glibc 2.28 (dist-workspace.toml) and must start and answer
# on RHEL 8-family, Debian 10 and Ubuntu 20.04. Debian 12 is where the 2.39-linked releases failed.
#
# Usage: scripts/check-linux-compat.sh BINARY
# Needs docker and network (each container installs git from its distribution's archive first: KnowCode runs git).
# Each image runs on the host's own architecture, so BINARY must match it. The repository is mounted read-only, since
# KnowCode never writes into an indexed repository.
set -euo pipefail
binary=$(cd "$(dirname "${1:?usage: check-linux-compat.sh BINARY}")" && pwd -P)/$(basename "$1")
[ -x "$binary" ] || { echo "check-linux-compat: $binary is not an executable file" >&2; exit 2; }

# Multi-architecture digests, so the same pin serves x86_64 and arm64 runners; each with the command installing git.
# Debian 10's packages moved to archive.debian.org when it reached end of life.
apt_git='apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends git >/dev/null'
images=(
    "rockylinux:8@sha256:9794037624aaa6212aeada1d28861ef5e0a935adaf93e4ef79837119f2a2d04c|dnf -q -y install git-core >/dev/null"
    "debian:10@sha256:58ce6f1271ae1c8a2006ff7d3e54e9874d839f573d8009c20154ad0f2fb0a225|printf 'deb http://archive.debian.org/debian buster main\\n' > /etc/apt/sources.list && $apt_git"
    "ubuntu:20.04@sha256:8feb4d8ca5354def3d8fce243717141ce31e2c428701f6682bd2fafe15388214|$apt_git"
    "debian:12@sha256:d74df7598108a173a621eda4d58a2adbf850b2d9a9d81bc2cca1a6c21174a880|$apt_git"
)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
repo="$work/app"
mkdir "$repo"
printf 'export function apiHandler() { return 42; }\n' > "$repo/api.ts"
printf 'import { apiHandler } from "./api";\nexport function route() { return apiHandler(); }\n' > "$repo/route.ts"
git -C "$repo" init -q
git -C "$repo" add .
git -C "$repo" -c user.name="Compat Check" -c user.email=compat@example.test commit -qm "tiny repository"

# Runs inside each container as plain sh, after git is installed. The repository belongs to the host's user, so git
# is told to trust it.
# shellcheck disable=SC2016 # expanded by the container's shell, not this one
probe='set -eu
git config --global --add safe.directory /repo
export KNOWCODE_CACHE_DIR=/tmp/kc/cache KNOWCODE_CONFIG_DIR=/tmp/kc/config KNOWCODE_DATA_DIR=/tmp/kc/data
export KNOWCODE_UPDATE_CHECK=0
cd /repo
/kc/knowcode --version
found=$(/kc/knowcode find apiHandler)
case "$found" in *api.ts*) ;; *) echo "find apiHandler did not name api.ts:"; echo "$found"; exit 1 ;; esac
refs=$(/kc/knowcode refs apiHandler)
case "$refs" in *route.ts*) ;; *) echo "refs apiHandler did not name route.ts:"; echo "$refs"; exit 1 ;; esac
echo "indexed, found apiHandler in api.ts and its use in route.ts"'

failed=0
for entry in "${images[@]}"; do
    image=${entry%%|*}
    install=${entry#*|}
    echo "== ${image%@*}"
    if out=$(docker run --rm -e HOME=/tmp/kc -v "$binary:/kc/knowcode:ro" -v "$repo:/repo:ro" "$image" \
        sh -c "set -e; mkdir -p /tmp/kc; $install; $probe" 2>&1); then
        printf '%s\n' "$out" | sed 's/^/   /'
        echo "   PASS"
    else
        printf '%s\n' "$out" | sed 's/^/   /'
        echo "   FAIL"
        failed=1
    fi
done
exit $failed
