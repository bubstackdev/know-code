# compare-v1's step 5 enumerations

Every reference and declaration the toolchain sees in step 5's six fresh repositories, at their pins in
[repos.txt](../repos.txt), with compare-v1's field `in` (eval/compare-v1/README.md, step 5). [questions.py](../../../../scripts/eval/compare/questions.py)
draws step 5's questions and labels from these files and nothing else.

The files (about 9 MB compressed) aren't checked in, as identity-v1's aren't. [SHA256SUMS](SHA256SUMS) holds their
digests as labelled. They are made with identity-v2's labellers, run in their own containers by
[run.sh](../../../../scripts/eval/identity/run.sh), with `REPOS` and `BUILDS` pointing at this directory's lists and
with `LABEL_ENCLOSING=1`:

```sh
for REPO in bubbletea gorm WxJava HikariCP ksp kotlin-logging; do
  read -r _ _ URL SHA < <(grep -E "^$REPO " eval/compare-v1/fresh/repos.txt)
  git init -q WORK/clones/$REPO && git -C WORK/clones/$REPO fetch -q --depth 1 "$URL" "$SHA" \
    && git -C WORK/clones/$REPO checkout -q FETCH_HEAD
  REPOS=eval/compare-v1/fresh/repos.txt BUILDS=eval/compare-v1/fresh/builds.txt LABEL_ENCLOSING=1 \
    scripts/eval/identity/run.sh $REPO WORK/clones/$REPO IO/$REPO   # IO/$REPO/out/enumeration-$REPO.jsonl.gz
done
python3 scripts/eval/compare/questions.py draw IO/*/out/enumeration-*.jsonl.gz \
  --out eval/compare-v1/questions-step5.jsonl --strata eval/compare-v1/strata-step5.json \
  --lang bubbletea=go --lang gorm=go --lang WxJava=java --lang HikariCP=java \
  --lang ksp=kotlin --lang kotlin-logging=kotlin                                      # the committed draw
```

Go's files are made again byte for byte: bubbletea and gorm, each labelled three times, gave the same digests. A scip-java
rebuild can reorder the rows of a Java or Kotlin file, so its digest can differ while its rows don't. The draw doesn't
depend on row order: drawing again from all six files with their rows shuffled gives the committed questions and
strata byte for byte.
