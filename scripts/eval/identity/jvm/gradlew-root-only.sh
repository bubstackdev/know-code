#!/usr/bin/env bash
# The container's gradlew, when builds.txt sets SCIP_ROOT_BUILD_ONLY=1 (eval/identity-v2/README.md): scip-java's
# generated init script applies its plugin to every project of every build, buildSrc's and included builds' too, and
# its Kotlin part reads each compile's arguments while the build is configured, which Gradle refuses for a build of
# precompiled script plugins. Those builds are the repository's build logic, never labelled, so before the real
# wrapper runs, the init script is kept to the root build: a project whose build has a parent build is skipped.
set -euo pipefail
args=("$@")
for ((i = 0; i + 1 < ${#args[@]}; i++)); do
  if [ "${args[$i]}" = --init-script ]; then
    f=${args[$((i + 1))]}
    if grep -q 'apply plugin: ScipGradlePlugin' "$f" && ! grep -q 'scip-java: root build only' "$f"; then
      sed -i -E 's/^([[:space:]]*)allprojects \{/&\n\1  if (project.gradle.parent != null) return \/\/ scip-java: root build only/' "$f"
      grep -q 'scip-java: root build only' "$f"
    fi
  fi
done
exec "$(dirname "$0")/gradlew.real" "$@"
