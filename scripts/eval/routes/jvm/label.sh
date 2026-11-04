#!/usr/bin/env bash
# Labels one routes-v1 Spring repository in a throwaway Linux container (eval/routes-v1/README.md, "Java and Kotlin").
#
#     jvm/label.sh REPO CLONE IO        (run.sh's `jvm` arm)
#
# The clone's tracked files and this directory are copied to IO/in, which the container mounts read-only; the
# container copies them into its own filesystem. With the network up, the repository's own build compiles each listed
# module's main code and writes its runtime classpath; then the container is disconnected (and checked to no longer
# resolve names), and for each module KcMappings (Spring MVC) and KcFluxMappings (WebFlux), whichever the module's
# classpath holds, are compiled against that classpath and list its request mappings. Only IO/out is writable: the
# listing comes out as IO/out/routes-REPO.jsonl (run.sh's row format: `program` and `route` rows) and the log as
# IO/out/REPO.log. The container is removed afterwards. CPUS (default 4) and MEMORY (default 8g) limit it.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$1 CLONE=$2 IO=$3
CPUS=${CPUS:-4}
MEMORY=${MEMORY:-8g}
IMAGE=$(awk -v r="$REPO" '$1 == r { print $2 }' "$HERE/images.txt")
[ -n "$IMAGE" ] || { echo "label.sh: no image for $REPO in images.txt" >&2; exit 2; }

rm -rf "$IO/in" && mkdir -p "$IO/in" "$IO/out"
(cd "$CLONE" && git -c core.hooksPath=/dev/null ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -cf "$IO/in/src.tar" -T -)
cp -R "$HERE" "$IO/in/tools"
awk -v r="$REPO" '$1 == r { print $2, $3 }' "$HERE/programs.txt" > "$IO/in/programs.txt"
BUILD=$(awk 'NR == 1 { print $1 }' "$IO/in/programs.txt")
MODULES=$(awk '{ print $2 }' "$IO/in/programs.txt" | tr '\n' ' ')

NAME=kc-routes-$REPO-$$
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker create --name "$NAME" --cpus "$CPUS" --memory "$MEMORY" --network bridge \
  -v "$(cd "$IO/in" && pwd):/in:ro" -v "$(cd "$IO/out" && pwd):/out" \
  --entrypoint sleep "$IMAGE" infinity >/dev/null
docker start "$NAME" >/dev/null
run() { docker exec "$NAME" bash -euo pipefail -c "$1"; }
LOG=/out/$REPO.log

# 1. Copy in; compile the modules' main code and write their runtime classpaths, with the network up. A Maven
#    front-end build (frontend-maven-plugin) is skipped: it builds no Java.
run "mkdir -p /work/src && tar -xf /in/src.tar -C /work/src && cp -R /in/tools /work/tools && : > $LOG
     java -version >> $LOG 2>&1
     cd /work/src
     if [ '$BUILD' = maven ]; then
       MVN=mvn; if [ -f mvnw ]; then chmod +x mvnw; fi
       PL=''; for m in $MODULES; do if [ \"\$m\" != . ]; then PL=\"\$PL,\$m\"; fi; done
       mvn -B -DskipTests -Dskip.npm -Dskip.installnodenpm -Dskip.yarn -Dcheckstyle.skip -Dmodernizer.skip -Djacoco.skip \
         \${PL:+-pl \${PL#,} -am} compile dependency:build-classpath -Dmdep.outputFile=kc-cp.txt -Dmdep.includeScope=test >>$LOG 2>&1
     else
       chmod +x gradlew
       ./gradlew --no-daemon --no-configuration-cache -q -I /work/tools/kc-classpath.init.gradle classes kcClasspath >>$LOG 2>&1
     fi"

# 2. Off the network: list each module's mappings.
docker network disconnect bridge "$NAME"
run "! getent hosts github.com >/dev/null" || { echo "label.sh: the container still resolves names" >&2; exit 1; }
run "cd /work/src && : > /out/routes-$REPO.jsonl
     while read -r build mod; do
       dir=\$mod; [ \"\$dir\" = . ] && dir=''
       base=/work/src/\${dir:+\$dir/}
       if [ \"\$build\" = maven ]; then classes=\${base}target/classes; cp=\$(cat \${base}kc-cp.txt 2>/dev/null || true):\$classes
       else classes=\$(cat \${base}kc-classes.txt 2>/dev/null || true); cp=\$(cat \${base}kc-cp.txt 2>/dev/null || true); fi
       # The directories of the module's main code: the code the listing covers.
       dirs=\$(cd /work/src && find \${dir:-.}/src/main -type f \\( -name '*.java' -o -name '*.kt' \\) 2>/dev/null | sed 's#^\\./##; s#/[^/]*\$##' | sort -u | tr '\n' ' ')
       status=listed
       found=0
       for lister in KcMappings:spring-webmvc KcFluxMappings:spring-webflux; do
         cls=\${lister%%:*}; jar=\${lister#*:}
         case \"\$cp\" in *\$jar*) ;; *) continue ;; esac
         out=/work/kc/\$cls-\$(echo \$mod | tr / _)
         mkdir -p \$out
         if javac -nowarn -cp \"\$cp\" -d \$out /work/tools/\$cls.java >>$LOG 2>&1 \
            && java -cp \"\$cp:\$out\" \$cls \$(echo \$classes | tr ':' ' ') > \$out/rows 2>>$LOG; then
           found=1
           sed \"s#^{#{\\\"t\\\": \\\"route\\\", \\\"repo\\\": \\\"$REPO\\\", \\\"framework\\\": \\\"spring\\\", \\\"module\\\": \\\"\$mod\\\", \\\"package\\\": \\\"\$mod\\\", #\" \$out/rows >> /out/routes-$REPO.jsonl
         else
           status=failed; echo \"listing \$mod with \$cls failed\" >> $LOG
         fi
       done
       [ \$found = 0 ] && [ \$status = listed ] && echo \"\$mod: no Spring web module on its classpath\" >> $LOG
       printf '{\"t\": \"program\", \"repo\": \"%s\", \"module\": \"%s\", \"package\": \"%s\", \"status\": \"%s\", \"dirs\": \"%s\"}\n' \
         $REPO \"\$mod\" \"\$mod\" \$status \"\$dirs\" >> /out/routes-$REPO.jsonl
     done < /in/programs.txt"
echo "$REPO: $(grep -c '"t": "route"' "$IO/out/routes-$REPO.jsonl") route rows"
