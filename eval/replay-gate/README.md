# Release replay gate

[replay_gate.py](../../scripts/eval/replay_gate.py) checks a KnowCode build's answers on the calls agents really
made, with no model calls. It is the "answers on recorded calls" check of the [0.4 gate](../../docs/plans/0.4.md#gate).

## The corpus

[corpus.jsonl](corpus.jsonl) holds every KnowCode call in the transcripts of the published public-v1 runs (baseline,
fifth, final, rerun) and public-v2 runs (baseline, release candidate). That is 1,092 calls over 64 questions, kept as
717 distinct rows.

- **One row per question and distinct call:** the tool the call ran and its arguments, and how many sessions made
  it.
- **`kc` calls:** a `kc {op: …}` call is stored as its op's tool.
- **Paths:** session paths are made repository-relative.
- **Rebuilding it:** `replay_gate.py extract` from the raw transcripts. Those aren't published, because they hold
  build-machine paths.
- **Not included:** public-v3's pilot calls. public-v3 stays a holdout for the next milestone study. A local corpus
  can be passed with `--corpus`.

## What a run does

1. **Workspaces:** neutral Git workspaces from the pinned public source (public-v2's cohort, which pins public-v1's
   six repositories at the same commits). They are APFS clones, each hashed against the source receipt, and made once.
2. **Index:** the build indexes them into its own cache.
3. **Replay:** each call is sent to `knowcode serve --tools all` over MCP. Recorded `kc_more` calls are skipped,
   because their cursors belong to the recorded session; `kc_note_add` is skipped, because it writes.
4. **Scoring:** per question, the places (`repo/path:line`) its calls' answers name are scored against the study's
   published label with [score.py](../../scripts/eval/neutral/score.py).
5. **Report:** recall and precision per question type, answer bytes and errors per tool, and the tool schemas per
   profile:
   - tools listed and always loaded, and the instructions, in bytes;
   - and in tokens through the provider's count_tokens when `ANTHROPIC_API_KEY` is set. CI has no key, so CI
     compares bytes.

`compare` exits 1 on a regression beyond a tolerance:

| Check | Fails when the candidate |
| --- | --- |
| Recall per question type | drops by more than 0.02 |
| Precision per question type | drops by more than 0.05 |
| Errors | errors on a call the base answered |
| Answer bytes per tool | grow more than 10% |
| Always-loaded schemas and instructions | grow more than 5% |

Precision here is over every place the answers name, context and evidence included, so it is low in absolute terms. It
is for comparing builds, not a claim about answers.

## Running it

```sh
replay_gate.py run --knowcode BASE_BIN --label v0.3.0 --source SOURCE --work WORK
replay_gate.py run --knowcode CANDIDATE_BIN --label candidate --source SOURCE --work WORK
replay_gate.py compare WORK/builds/<base hash>/summary.json WORK/builds/<candidate hash>/summary.json
```

`SOURCE` is the public source as `scripts/prepare-public-eval.py` exports it for public-v2's manifest. `WORK` holds the
workspaces and each build's cache, under a directory named by the binary's hash. Nothing is written into a user's
repository.

**How long it takes**, on a shared development Mac under load:

- **Full corpus:** about 2 minutes per build: 29–41 s indexing the ten repositories, 74–84 s for 686 calls. The first
  run also takes 19 s to prepare the workspaces.
- **Repeatable:** two runs of one build give byte-identical per-call results.
- **Fast subset:** `--repos autofac,autofac-di,serilog,serilog-extensions-hosting,serilog-aspnetcore` keeps the 22
  questions whose repositories are all small (about 12 MB of source, 119 calls). It takes about 6 s per build.

**In CI:** the `replay` job of [ci.yml](../../.github/workflows/ci.yml) runs the fast subset on every pull request, on
Linux. It compares the latest release's `x86_64-unknown-linux-gnu` asset, checked against its published checksum, with
the change's debug build. `replay_gate.py fetch` exports the five repositories' pinned public source, cached by the
manifest that pins them. The comparison goes in the job summary, and the job fails on a regression beyond tolerance.

## First replay: v0.3.0 against main

v0.3.0 is the release asset (sha256 `b7ab63ba…`); main is d82fa18, built with `cargo build --release`. Both report
`knowcode 0.3.0`.

**Answers:**

| Question type | Questions | Recall | Precision |
| --- | ---: | ---: | ---: |
| definition lookup | 16 | 0.969 | 0.398 |
| DI implementations | 16 | 0.869 | 0.243 |
| HTTP callers across repositories | 16 | 0.312 | 0.019 → 0.020 |
| two-hop impact | 16 | 0.587 → 0.628 | 0.175 → 0.203 |

**Errors:** the same 15 `kc_search` calls error on both builds.

**Answer bytes:** `kc_impact` grew 5.1%, `kc_status` 0.2%, and `kc_context` shrank 0.1%; the rest are unchanged.

**Tool cost per turn** (always-loaded tools and instructions; tokens from count_tokens, model `claude-opus-5-5`):

| Build | Profile | Always loaded | Schema bytes | Instructions bytes | Tokens |
| --- | --- | ---: | ---: | ---: | ---: |
| v0.3.0 | default (`core`) | 8 | 2,631 | 611 | 1,276 |
| main | default (`one`) | 1 | 626 | 657 | 502 |
| main | `core` | 5 | 1,886 | 611 | 988 |

`compare`: no regression beyond tolerance.
