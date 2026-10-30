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
# QUESTIONS (default eval/compare-v1/questions.jsonl) is copied in as /in/questions.jsonl. HISTORY=1 copies the clone
# whole, its .git included, and keeps its history instead of committing the tree once: co-change questions read it.
# For Java and Kotlin, BUILDS (default identity-v2's jvm/builds.txt) gives the repository's restore build.
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
  cp "${QUESTIONS:-$HERE/../../../eval/compare-v1/questions.jsonl}" "$IO/in/questions.jsonl"
  if [ "${HISTORY:-0}" = 1 ]; then
    [ -z "$(git -C "$CLONE" status --porcelain)" ] || { echo "container.sh: $CLONE has local changes" >&2; exit 2; }
    (cd "$CLONE" && COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs -cf "$IO/in/src.tar" .)
  else
    (cd "$CLONE" && git -c core.hooksPath=/dev/null ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -cf "$IO/in/src.tar" -T -)
  fi
fi
(cd "$HERE/.." && COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --exclude __pycache__ -cf "$IO/in/harness.tar" compare)
case $LANGUAGE in
  java|kotlin)
    BUILDS=${BUILDS:-$HERE/../identity/jvm/builds.txt}
    mkdir -p "$IO/in/jvm" && cp "$HERE/../identity/jvm/no-werror.gradle" "$HERE/../identity/jvm/skip-buildsrc.gradle" "$IO/in/jvm/"
    [ "$REPO" = probe ] || LINE=$(grep -E "^$REPO " "$BUILDS") || { echo "container.sh: $REPO is not in $BUILDS" >&2; exit 2; } ;;
esac

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
SERENA_HOME=/work/state/serena  # tools.py's home for Serena under run.py's STATE

run "mkdir -p /work/harness /work/src && tar -xf /in/harness.tar -C /work/harness && : > $LOG
     { knowcode --version; codebase-memory-mcp --version; serena --version || true; } >>$LOG 2>&1"

# The tree as one commit of its tracked files, before anything is restored into it: KnowCode indexes git repositories,
# and every tool reads the same tree.
snapshot() {  # $1: the directory, in the container
  run "cd $1 && git init -q && git add -A && GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z \
       git -c user.name=compare-v1 -c user.email=compare-v1@example.invalid commit -q -m snapshot"
}

restore() {  # $1: the directory to restore, in the container
  case $LANGUAGE in
    csharp) run "cd $1 && dotnet restore -p:NuGetAudit=false \$(ls *.sln *.slnx 2>/dev/null | head -1) >>$LOG 2>&1" ;;
    typescript) run "cd $1 && if [ -f pnpm-lock.yaml ]; then corepack enable && pnpm install --frozen-lockfile --ignore-scripts;
                     elif [ -f package-lock.json ]; then npm ci --ignore-scripts; else npm install --ignore-scripts; fi >>$LOG 2>&1" ;;
    swift) run "cd $1 && swift package resolve >>$LOG 2>&1" ;;
    # Go, Java and Kotlin keep what they restore in the home (the module cache, ~/.m2, ~/.gradle), and each tool has
    # a home of its own (tools.py). They are restored into Serena's, the only tool whose language servers read them.
    go)
      # As identity-v2's labeller restores: every module's dependencies, tests' included.
      run "export HOME=$SERENA_HOME GOTOOLCHAIN=local && unset GOPATH && mkdir -p \$HOME && cd $1 && go version >>$LOG
           for m in \$(find . -name go.mod -not -path '*/vendor/*' -not -path '*/testdata/*' -not -path '*/.*/*'); do
             (cd \$(dirname \$m) && go mod download >>$LOG 2>&1 && go list -test -deps ./... >/dev/null 2>>$LOG) || echo \"go: restore failed in \$m\" >>$LOG
           done" ;;
    java|kotlin)
      # As identity-v2's labeller restores: its whole indexing build (builds.txt, with the same opt-ins), which fetches
      # every dependency and plugin. Then the labeller's Gradle settings leave Serena's home, and the tree goes back to
      # its one commit (the build's output and any version setting it changed), so every tool reads the same tree.
      local args=${LINE#* -- } jvm tool envs catalog
      read -r _ jvm tool envs <<<"${LINE%% -- *}"
      catalog=$(sed -nE 's/(^| )KOTLIN_CATALOG=([^ ]+).*/\2/p' <<<"$envs")
      run "JDKS=\$(coursier java-home --jvm 17),\$(coursier java-home --jvm 21),/opt/java/openjdk
           export COURSIER_CACHE=/root/.cache/coursier HOME=$SERENA_HOME && mkdir -p \$HOME/.gradle/init.d
           cp /in/jvm/*.gradle \$HOME/.gradle/init.d/ && echo org.gradle.java.installations.paths=\$JDKS > \$HOME/.gradle/gradle.properties
           cd $1
           if [ -n '$catalog' ]; then sed -i -E 's/^kotlin = \"[^\"]*\"/kotlin = \"$catalog\"/' gradle/libs.versions.toml; fi
           env $envs JVM_VERSION=$jvm scip-java index --build-tool=$tool -- $args >>$LOG 2>&1 || echo 'restore build failed' >>$LOG
           rm -rf \$HOME/.gradle/init.d \$HOME/.gradle/gradle.properties
           git clean -fdxq && git checkout -q -- . && test -z \"\$(git status --porcelain)\"" ;;
  esac
}

if [ "$REPO" = probe ]; then
  # Not scored: the network stays up, so each tool behaves as it would on a developer's machine.
  for tool in knowcode codebase-memory-mcp serena; do
    run "mkdir -p /work/probe/$tool && cp -R /work/harness/compare/fixture/$LANGUAGE /work/probe/$tool/$LANGUAGE"
    snapshot "/work/probe/$tool/$LANGUAGE"
    restore "/work/probe/$tool/$LANGUAGE"
  done
  run "cd /work/harness && compare-python -m compare.probe $LANGUAGE /out >>$LOG 2>&1"
  exit 0
fi

# 1. With the network up: restore, then every tool indexes. The restore is for Serena's language servers, which
# resolve through the dependencies; KnowCode and codebase-memory-mcp ignore them. A run that doesn't start Serena
# (--tools without it, as on locate and task trees) indexes the tree as checked out.
# Owned by the container's user, so git reads it (a clone's owner would otherwise be refused as dubious).
run "tar --no-same-owner -xf /in/src.tar -C /work/src"
[ "${HISTORY:-0}" = 1 ] || snapshot /work/src
case " $* " in
  *" --tools "*) case "$*" in *serena*) restore /work/src ;; esac ;;
  *) restore /work/src ;;
esac
run "cd /work/harness && compare-python -m compare.run index --language $LANGUAGE --repo $REPO --src /work/src --out /out $* >>$LOG 2>&1"

# 2. Off the network: ask.
docker network disconnect bridge "$NAME"
run "! getent hosts github.com >/dev/null" || { echo "container.sh: the container still resolves names" >&2; exit 1; }
run "cd /work/harness && compare-python -m compare.run ask --language $LANGUAGE --repo $REPO --src /work/src --out /out $* >>$LOG 2>&1"
