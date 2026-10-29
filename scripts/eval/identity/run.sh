#!/usr/bin/env bash
# Labels one identity-v1 repository in a throwaway Linux container (eval/identity-v1/README.md).
#
#     run.sh REPO CLONE IO
#
# REPO is a name in repos.txt, CLONE its pinned clone (read, never written), IO a scratch directory. The clone's
# tracked files and this directory's labellers are copied to IO/in, which the container mounts read-only;
# the container copies them into its own filesystem, restores the repository's dependencies with the network up,
# disconnects from the network, builds and labels. Only IO/out is writable: the enumeration and the logs come out
# there, as IO/out/enumeration-REPO.jsonl.gz and IO/out/REPO.log (and, for Java and Kotlin, the SCIP index the
# enumeration is read from, IO/out/index-REPO.scip). The container is removed afterwards.
#
# Images are pinned by digest (IMAGES below); CPUS (default 4) and MEMORY (default 12g) limit the container.
# LABEL_ENCLOSING=1 adds compare-v1's `in` field to each reference row (eval/compare-v1/README.md); without it the
# enumeration is identity-v1's.
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
GO_IMAGE=golang:1.27-bookworm@sha256:d9b6a1beb9474adc2a1ce842573639b44b2a0dd49b6669f51d3821baf9ec37c9
# scip-java with its JDKs (17, 21, 25), Maven and Gradle; the multi-platform index's digest.
JVM_IMAGE=ghcr.io/scip-code/scip-java:0.13.2@sha256:459c2fa0544d3801857428d94590e86bcd5f8af25980a36cb360720c1b3cf802

# REPOS and BUILDS (default repos.txt and jvm/builds.txt here) let another evaluation pin its own repositories without
# editing these lists: compare-v1's fresh repositories (eval/compare-v1/README.md).
REPOS=${REPOS:-$HERE/repos.txt}
BUILDS=${BUILDS:-$HERE/jvm/builds.txt}
read -r _ LANGUAGE URL SHA < <(grep -E "^$REPO " "$REPOS") || { echo "run.sh: $REPO is not in $REPOS" >&2; exit 2; }
if [ "$(git -C "$CLONE" rev-parse HEAD)" != "$SHA" ]; then
  echo "run.sh: $CLONE is not at $SHA" >&2
  exit 2
fi

rm -rf "$IO/in" && mkdir -p "$IO/in" "$IO/out"
(cd "$CLONE" && git -c core.hooksPath=/dev/null ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -cf "$IO/in/src.tar" -T -)
cp -R "$HERE/$LANGUAGE" "$IO/in/tools"
cp "$HERE/scip.py" "$IO/in/scip.py"
find "$IO/in/tools" \( -name bin -o -name obj \) -prune -exec rm -rf {} +

NAME=kc-identity-$REPO-$$
case $LANGUAGE in
  swift) IMAGE=$SWIFT_IMAGE ;;
  csharp) IMAGE=$DOTNET_IMAGE ;;
  typescript) IMAGE=$NODE_IMAGE ;;
  go) IMAGE=$GO_IMAGE ;;
  jvm) IMAGE=$JVM_IMAGE ;;
esac
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker create --name "$NAME" --cpus "$CPUS" --memory "$MEMORY" --network bridge \
  -v "$(cd "$IO/in" && pwd):/in:ro" -v "$(cd "$IO/out" && pwd):/out" \
  -e DOTNET_CLI_TELEMETRY_OPTOUT=1 -e DOTNET_NOLOGO=1 -e NUGET_XMLDOC_MODE=skip -e COREPACK_ENABLE_DOWNLOAD_PROMPT=0 \
  -e GOTOOLCHAIN=local -e GOFLAGS=-mod=mod -e CGO_ENABLED=1 \
  -e npm_config_ignore_scripts=true -e npm_config_audit=false -e npm_config_fund=false \
  -e LABEL_ENCLOSING="${LABEL_ENCLOSING:-0}" \
  --entrypoint sleep "$IMAGE" infinity >/dev/null
docker start "$NAME" >/dev/null
run() { docker exec "$NAME" bash -euo pipefail -c "$1"; }
LOG=/out/$REPO.log

# 1. Copy in, and restore with the network up.
run "mkdir -p /work/src && tar -xf /in/src.tar -C /work/src && cp -R /in/tools /work/tools && cp /in/scip.py /work/ && : > $LOG"
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
  go)
    # Every module's dependencies, tests' included, and the labeller's own (x/tools), before the network goes.
    # scip-go is the labeller's cross-check (eval/identity-v2/README.md, Go): installed here, run offline below.
    run "go version >>$LOG
         cd /work/tools && go mod download >>$LOG 2>&1 && go build -o /work/label . >>$LOG 2>&1
         GOBIN=/work/bin go install github.com/scip-code/scip-go/cmd/scip-go@v0.2.9 >>$LOG 2>&1
         cd /work/src && for m in \$(find . -name go.mod -not -path '*/vendor/*' -not -path '*/testdata/*' -not -path '*/.*/*'); do
           (cd \$(dirname \$m) && go mod download >>$LOG 2>&1 && go list -test -deps ./... >/dev/null 2>>$LOG) || echo \"go: restore failed in \$m\" >>$LOG
         done" ;;
  jvm)
    # builds.txt: NAME JVM TOOL [VAR=VALUE ...] -- BUILD ARGUMENTS. The restore is a whole indexing build with the
    # network up, so that every dependency, plugin and compiler is fetched; its output is thrown away.
    LINE=$(grep -E "^$REPO " "$BUILDS")
    ARGS=${LINE#* -- }
    read -r _ JVM TOOL ENVS <<<"${LINE%% -- *}"
    BUILD="env $ENVS JVM_VERSION=$JVM scip-java index --build-tool=$TOOL --"
    run "mkdir -p /root/.gradle/init.d && cp /work/tools/no-werror.gradle /root/.gradle/init.d/
         JDKS=\$(coursier java-home --jvm 17),\$(coursier java-home --jvm 21),/opt/java/openjdk
         echo org.gradle.java.installations.paths=\$JDKS > /root/.gradle/gradle.properties
         cd /work/src && $BUILD $ARGS >>$LOG 2>&1 || echo 'restore build failed' >>$LOG
         rm -f index.scip" ;;
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
  go)
    run "cd /work/src && GOPROXY=off GOFLAGS=-mod=mod /work/label -repo $REPO -src /work/src 2>>$LOG | gzip -n > /out/enumeration-$REPO.jsonl.gz
         mkdir -p /out/scip-$REPO && i=0
         for m in \$(find . -name go.mod -not -path '*/vendor/*' -not -path '*/testdata/*' -not -path '*/.*/*' | sort); do
           d=\$(dirname \$m); i=\$((i+1))
           (cd \$d && GOPROXY=off /work/bin/scip-go index --quiet ./... -o /out/scip-$REPO/\$i.scip 2>>$LOG) || echo \"scip-go failed in \$d\" >>$LOG
           echo \"\$i \${d#./}\" >> /out/scip-$REPO/modules.txt
         done" ;;
  jvm)
    OFFLINE=$([ "$TOOL" = maven ] && echo -o || echo --offline)
    run "cd /work/src && $BUILD $OFFLINE $ARGS >>$LOG 2>&1
         cp index.scip /out/index-$REPO.scip
         python3 /work/tools/label.py --repo $REPO --src /work/src index.scip 2>>$LOG | gzip -n > /out/enumeration-$REPO.jsonl.gz" ;;
esac
echo "run.sh: $REPO: $(gzip -dc "$IO/out/enumeration-$REPO.jsonl.gz" | wc -l | tr -d ' ') rows"
