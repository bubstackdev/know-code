#!/usr/bin/env bash
# Runs compare-v1 for one repository in a throwaway container (eval/compare-v1/README.md, "The container").
#
#     container.sh LANGUAGE REPO CLONE IO [run.py arguments ...]
#     container.sh LANGUAGE probe - IO                     # the fixture probe (probe.py), before the freeze
#
# The clone's tracked files and the harness (scripts/eval) are copied into IO/in, which the container mounts
# read-only; only IO/out is writable. With the network up, the repository's dependencies are restored (as identity-v1's
# labeller restores them) and every tool indexes it. The container is then disconnected, the run checks that names no
# longer resolve, and the questions are asked. The container is removed afterwards, whatever happens.
#
# CPUS (default 4) and MEMORY (default 12g) limit the container; the image is kc-compare-v1:LANGUAGE (build.sh).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
LANGUAGE=${1:?usage: container.sh LANGUAGE REPO CLONE IO [args ...]}
REPO=${2:?}
CLONE=${3:?}
IO=${4:?}
shift 4
CPUS=${CPUS:-4}
MEMORY=${MEMORY:-12g}

rm -rf "$IO/in" && mkdir -p "$IO/in" "$IO/out"
IO=$(cd "$IO" && pwd)
if [ "$REPO" != probe ]; then
  (cd "$CLONE" && git -c core.hooksPath=/dev/null ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -cf "$IO/in/src.tar" -T -)
fi
(cd "$HERE/.." && COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --exclude __pycache__ -cf "$IO/in/harness.tar" compare)

NAME=kc-compare-$LANGUAGE-$$
cleanup() { docker rm -f -v "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker create --name "$NAME" --cpus "$CPUS" --memory "$MEMORY" --network bridge \
  -v "$IO/in:/in:ro" -v "$IO/out:/out" -e npm_config_ignore_scripts=true -e npm_config_audit=false \
  -e npm_config_fund=false -e COREPACK_ENABLE_DOWNLOAD_PROMPT=0 -e PYTHONPATH=/work/harness -e PYTHONDONTWRITEBYTECODE=1 \
  --entrypoint sleep "kc-compare-v1:$LANGUAGE" infinity >/dev/null
docker start "$NAME" >/dev/null
run() { docker exec "$NAME" bash -euo pipefail -c "$1"; }
LOG=/out/container.log

run "mkdir -p /work/harness /work/src && tar -xf /in/harness.tar -C /work/harness && : > $LOG
     { knowcode --version; codebase-memory-mcp --version; serena --version || true; } >>$LOG 2>&1"

restore() {  # $1: the directory to restore, in the container
  case $LANGUAGE in
    csharp) run "cd $1 && dotnet restore -p:NuGetAudit=false \$(ls *.sln *.slnx 2>/dev/null | head -1) >>$LOG 2>&1" ;;
    typescript) run "cd $1 && if [ -f pnpm-lock.yaml ]; then corepack enable && pnpm install --frozen-lockfile --ignore-scripts;
                     elif [ -f package-lock.json ]; then npm ci --ignore-scripts; else npm install --ignore-scripts; fi >>$LOG 2>&1" ;;
    swift) run "cd $1 && swift package resolve >>$LOG 2>&1" ;;
  esac
}

if [ "$REPO" = probe ]; then
  # Not scored: the network stays up, so each tool behaves as it would on a developer's machine.
  for tool in knowcode codebase-memory-mcp serena; do
    run "mkdir -p /work/probe/$tool && cp -R /work/harness/compare/fixture/$LANGUAGE /work/probe/$tool/$LANGUAGE"
    restore "/work/probe/$tool/$LANGUAGE"
  done
  run "cd /work/harness && compare-python -m compare.probe $LANGUAGE /out >>$LOG 2>&1"
  exit 0
fi

# 1. With the network up: restore, then every tool indexes.
run "tar -xf /in/src.tar -C /work/src"
restore /work/src
run "cd /work/harness && compare-python -m compare.run index --language $LANGUAGE --repo $REPO --src /work/src --out /out $* >>$LOG 2>&1"

# 2. Off the network: ask.
docker network disconnect bridge "$NAME"
run "! getent hosts github.com >/dev/null" || { echo "container.sh: the container still resolves names" >&2; exit 1; }
run "cd /work/harness && compare-python -m compare.run ask --language $LANGUAGE --repo $REPO --src /work/src --out /out $* >>$LOG 2>&1"
