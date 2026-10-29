#!/usr/bin/env bash
# Labels one routes-v1 repository's server routes in a throwaway Linux container (eval/routes-v1/README.md).
#
#     run.sh REPO CLONE IO
#
# REPO is a name in repos.txt, CLONE its pinned clone (read, never written), IO a scratch directory. The clone's
# tracked files and the family's labeller are copied to IO/in, which the container mounts read-only; the container
# copies them into its own filesystem, runs the repository's harness/REPO/prepare.sh there if it has one, downloads
# the modules the listed programs need with the network up, disconnects, and lists each program's routes
# (programs.txt). Only IO/out is writable: the listing and the log come
# out there, as IO/out/routes-REPO.jsonl and IO/out/REPO.log. The container is removed afterwards.
#
# The image is pinned by digest; CPUS (default 4) and MEMORY (default 8g) limit the container.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=${1:?usage: run.sh REPO CLONE IO}
CLONE=$(cd "${2:?usage: run.sh REPO CLONE IO}" && pwd)
IO=${3:?usage: run.sh REPO CLONE IO}
CPUS=${CPUS:-4}
MEMORY=${MEMORY:-8g}

GO_IMAGE=golang:1.27-bookworm@sha256:d9b6a1beb9474adc2a1ce842573639b44b2a0dd49b6669f51d3821baf9ec37c9

read -r _ FAMILY URL SHA < <(grep -E "^$REPO " "$HERE/repos.txt") || { echo "run.sh: $REPO is not in repos.txt" >&2; exit 2; }
if [ "$(git -C "$CLONE" rev-parse HEAD)" != "$SHA" ]; then
  echo "run.sh: $CLONE is not at $SHA" >&2
  exit 2
fi
case $FAMILY in
  go) IMAGE=$GO_IMAGE ;;
  *) echo "run.sh: no labeller for $FAMILY" >&2; exit 2 ;;
esac

rm -rf "$IO/in" && mkdir -p "$IO/in" "$IO/out"
(cd "$CLONE" && git -c core.hooksPath=/dev/null ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -cf "$IO/in/src.tar" -T -)
cp -R "$HERE/$FAMILY" "$IO/in/tools"
grep -E "^$REPO " "$HERE/$FAMILY/programs.txt" | cut -d' ' -f2- > "$IO/in/programs.txt"

NAME=kc-routes-$REPO-$$
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker create --name "$NAME" --cpus "$CPUS" --memory "$MEMORY" --network bridge \
  -v "$(cd "$IO/in" && pwd):/in:ro" -v "$(cd "$IO/out" && pwd):/out" \
  -e GOTOOLCHAIN=local -e GOFLAGS=-mod=mod -e CGO_ENABLED=1 \
  --entrypoint sleep "$IMAGE" infinity >/dev/null
docker start "$NAME" >/dev/null
run() { docker exec "$NAME" bash -euo pipefail -c "$1"; }
LOG=/out/$REPO.log

# 1. Copy in, and download every listed module's dependencies (tests' included, for a test harness) with the network up.
run "mkdir -p /work/src /work/ov && tar -xf /in/src.tar -C /work/src && cp -R /in/tools /work/tools && : > $LOG
     go version >> $LOG
     if [ -f /work/tools/harness/$REPO/prepare.sh ]; then (cd /work/src && sh /work/tools/harness/$REPO/prepare.sh) >>$LOG 2>&1; fi
     for m in \$(cut -d' ' -f1 /in/programs.txt | sort -u); do
       (cd /work/src/\$m && go mod download >>$LOG 2>&1 && go list -test -deps ./... >/dev/null 2>>$LOG) || echo \"restore failed in \$m\" >>$LOG
     done"

# 2. Off the network: patch, build and list.
docker network disconnect bridge "$NAME"
run "! getent hosts github.com >/dev/null" || { echo "run.sh: the container still resolves names" >&2; exit 1; }
run "sh /work/tools/patch.sh >>$LOG 2>&1
     : > /out/routes-$REPO.jsonl
     while read -r mod pkg args; do
       if [ \"\$pkg\" = auto ]; then
         pkgs=\$(cd /work/src/\$mod && go list -f '{{if eq .Name \"main\"}}{{.ImportPath}}{{end}}' ./... 2>>$LOG | sed \"s#^\$(cd /work/src/\$mod && go list -m)/##; s#^\$(cd /work/src/\$mod && go list -m)\\\$#.#\")
       else
         pkgs=\$pkg
       fi
       for p in \$pkgs; do
         sh /work/tools/list.sh $REPO \"\$mod\" \"\$p\" \$args >> /out/routes-$REPO.jsonl 2>>$LOG
       done
     done < /in/programs.txt"
echo "run.sh: $REPO: $(grep -c '"t": "route"' "$IO/out/routes-$REPO.jsonl") routes from $(grep -c '"status": "listed"' "$IO/out/routes-$REPO.jsonl") of $(grep -c '"t": "program"' "$IO/out/routes-$REPO.jsonl") programs"
