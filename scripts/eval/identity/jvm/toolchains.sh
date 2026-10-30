#!/usr/bin/env bash
# Run in the labelling container with the network up (run.sh): fetches each JDK a build's toolchains ask for beyond the
# container's own, as ID@SHA256 (coursier's JVM id, then the sha256 of the archive it downloads), checks the archive,
# and prints the JDKs' homes, each after a comma, for Gradle's toolchain paths.
set -euo pipefail
for t in "$@"; do
  id=${t%@*} sum=${t#*@}
  home=$(coursier java-home --jvm "$id")
  # coursier unpacks the archive it downloaded (under v1/) into arc/, at the same path.
  archive=$(dirname "$home")
  archive=${archive/\/arc\//\/v1\/}
  echo "$sum  $archive" | sha256sum -c --quiet >&2
  printf ',%s' "$home"
done
