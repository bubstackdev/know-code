#!/usr/bin/env bash
# Labels one identity-v1 repository in a throwaway Linux container (eval/identity-v1/README.md).
#
#     run.sh REPO CLONE IO
#
# REPO is a name in repos.txt, CLONE its pinned clone (read, never written), IO a scratch directory. The clone's
# tracked files and this directory's labellers are copied to IO/in, which the container mounts read-only;
# the container copies them into its own filesystem, restores the repository's dependencies with the network up,
# disconnects from the network, builds and labels. Only IO/out is writable: the enumeration and the logs come out
# there, as IO/out/enumeration-REPO.jsonl.gz and IO/out/REPO.log. The container is removed afterwards.
#
# Images are pinned by digest (IMAGES below); CPUS (default 4) and MEMORY (default 12g) limit the container.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=${1:?usage: run.sh REPO CLONE IO}
CLONE=$(cd "${2:?usage: run.sh REPO CLONE IO}" && pwd)
IO=${3:?usage: run.sh REPO CLONE IO}
CPUS=${CPUS:-4}
MEMORY=${MEMORY:-12g}

# The toolchain images, by digest; README.md names their versions.
SWIFT_IMAGE=swift:6.3-noble@sha256:1038d54c80063f5cdfabc4560990c30af64b5de8ebf28161deb22ca4ad5db866
# The .NET SDK comes from Ubuntu's archive (dotnet-sdk-10.0), installed while the network is up.
DOTNET_IMAGE=ubuntu:26.04@sha256:19fcd1bf268f02b60aca3bd30bd8a6d9a11759a430e32921833e65f16d87348a
NODE_IMAGE=node:24-bookworm@sha256:6449e19d8184d84062525b6a7524def03146c2cb04a7f08b076680d0e8e4240a

read -r _ LANGUAGE URL SHA < <(grep -E "^$REPO " "$HERE/repos.txt") || { echo "run.sh: $REPO is not in repos.txt" >&2; exit 2; }
if [ "$(git -C "$CLONE" rev-parse HEAD)" != "$SHA" ]; then
  echo "run.sh: $CLONE is not at $SHA" >&2
  exit 2
fi

rm -rf "$IO/in" && mkdir -p "$IO/in" "$IO/out"
(cd "$CLONE" && git -c core.hooksPath=/dev/null ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -cf "$IO/in/src.tar" -T -)
cp -R "$HERE/$LANGUAGE" "$IO/in/tools"
find "$IO/in/tools" \( -name bin -o -name obj \) -prune -exec rm -rf {} +

NAME=kc-identity-$REPO-$$
case $LANGUAGE in
  swift) IMAGE=$SWIFT_IMAGE ;;
  csharp) IMAGE=$DOTNET_IMAGE ;;
  typescript) IMAGE=$NODE_IMAGE ;;
esac
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker create --name "$NAME" --cpus "$CPUS" --memory "$MEMORY" --network bridge \
  -v "$(cd "$IO/in" && pwd):/in:ro" -v "$(cd "$IO/out" && pwd):/out" \
  -e DOTNET_CLI_TELEMETRY_OPTOUT=1 -e DOTNET_NOLOGO=1 -e NUGET_XMLDOC_MODE=skip -e COREPACK_ENABLE_DOWNLOAD_PROMPT=0 \
  -e npm_config_ignore_scripts=true -e npm_config_audit=false -e npm_config_fund=false \
  --entrypoint sleep "$IMAGE" infinity >/dev/null
docker start "$NAME" >/dev/null
run() { docker exec "$NAME" bash -euo pipefail -c "$1"; }
LOG=/out/$REPO.log

# 1. Copy in, and restore with the network up.
run "mkdir -p /work/src && tar -xf /in/src.tar -C /work/src && cp -R /in/tools /work/tools && : > $LOG"
case $LANGUAGE in
  swift)
    run "command -v python3 >/dev/null || (apt-get update -qq && apt-get install -y -qq python3 >/dev/null) >>$LOG 2>&1
         cd /work/src && swift package resolve >>$LOG 2>&1" ;;
  csharp)
    PROPS=$(grep -E "^$REPO( |$)" "$HERE/csharp/properties.txt" | cut -s -d' ' -f2- || true)
    run "(apt-get update -qq && apt-get install -y -qq --no-install-recommends dotnet-sdk-10.0 ca-certificates >/dev/null) >>$LOG 2>&1
         cd /work/src && dotnet --info >>$LOG 2>&1
         dotnet restore -p:NuGetAudit=false $(for p in $PROPS; do printf -- "-p:%s " "$p"; done)\$(ls *.sln *.slnx 2>/dev/null | head -1) >>$LOG 2>&1
         dotnet restore /work/tools/Labeller.csproj >>$LOG 2>&1" ;;
  typescript)
    run "cd /work/src && node --version >>$LOG
         if [ -f pnpm-lock.yaml ]; then corepack enable >>$LOG 2>&1 && pnpm install --frozen-lockfile --ignore-scripts >>$LOG 2>&1;
         else npm ci --ignore-scripts >>$LOG 2>&1; fi" ;;
esac

# 2. Off the network: build and label.
docker network disconnect bridge "$NAME"
run "! getent hosts github.com >/dev/null" || { echo "run.sh: the container still resolves names" >&2; exit 1; }
case $LANGUAGE in
  swift)
    run "cd /work/src && (swift build --build-tests --enable-index-store -j $CPUS >>$LOG 2>&1 || echo 'swift build failed: labelling what was indexed' >>$LOG)
         clang -O1 -o /work/indexdump /work/tools/indexdump.c -L/usr/lib -lIndexStore >>$LOG 2>&1
         STORE=\$(find .build -type d -path '*/index/store' | head -1)
         /work/indexdump \$STORE > /work/dump.tsv 2>>$LOG
         python3 /work/tools/label.py --repo $REPO --src /work/src /work/dump.tsv | gzip -n > /out/enumeration-$REPO.jsonl.gz" ;;
  csharp)
    run "cd /work/tools && dotnet build -c Release --no-restore -o /work/labeller >>$LOG 2>&1
         cd /work/src && dotnet /work/labeller/Labeller.dll --repo $REPO --src /work/src --solution \$(ls *.sln *.slnx 2>/dev/null | head -1) $(for p in $PROPS; do printf -- "--property %s " "$p"; done)2>>$LOG | gzip -n > /out/enumeration-$REPO.jsonl.gz" ;;
  typescript)
    CONFIGS=$(grep -E "^$REPO " "$HERE/typescript/configs.txt" | cut -d' ' -f2-)
    run "cd /work/src && node /work/tools/label.cjs --repo $REPO --src /work/src $CONFIGS 2>>$LOG | gzip -n > /out/enumeration-$REPO.jsonl.gz" ;;
esac
echo "run.sh: $REPO: $(gzip -dc "$IO/out/enumeration-$REPO.jsonl.gz" | wc -l | tr -d ' ') rows"
