# compare-v1's enumerations

Every reference and declaration the toolchain sees in identity-v1's eight repositories, at identity-v1's pins
([repos.txt](../../../scripts/eval/identity/repos.txt)), with one field added: `in`, the key of the innermost
declaration around each reference (eval/compare-v1/README.md, "Labels"). [questions.py](../../../scripts/eval/compare/questions.py)
draws compare-v1's questions and labels from these files and nothing else.

The files (about 11 MB compressed) aren't checked in, as identity-v1's aren't. [SHA256SUMS](SHA256SUMS) holds
their digests, and they are made again with identity-v1's own labellers, run in their own containers
([run.sh](../../../scripts/eval/identity/run.sh)), with `LABEL_ENCLOSING=1`, from identity-v1's pinned clones:

```sh
python3 scripts/eval/identity_gate.py fetch --work WORK          # the pinned clones, WORK/clones/REPO
for REPO in serilog autofac jellyfin-server jellyfin-web hono swift-argument-parser swift-collections swift-nio; do
  LABEL_ENCLOSING=1 scripts/eval/identity/run.sh $REPO WORK/clones/$REPO IO/$REPO   # IO/$REPO/out/enumeration-$REPO.jsonl.gz
done
shasum -a 256 IO/*/out/enumeration-*.jsonl.gz                   # each digest as SHA256SUMS gives it
python3 scripts/eval/compare/questions.py check IO/*/out/enumeration-*.jsonl.gz          # the stop rule
python3 scripts/eval/compare/questions.py draw IO/*/out/enumeration-*.jsonl.gz \
  --out eval/compare-v1/questions.jsonl --strata eval/compare-v1/strata.json             # the committed draw
```

Without that variable the labellers write identity-v1's enumerations unchanged. Every other field of every row is
identity-v1's: `questions.py check` draws identity-v1's labels again from these files, and all 4,943 come back
unchanged (0 missing, 0 changed, 0 added). Labelling serilog and swift-argument-parser a second time gave
byte-identical files.

identity-v1's own files in [eval/identity-v1](../../identity-v1/README.md) are untouched.
