# Public evaluation design

No scored runs have been performed for this cohort. The [manifest](manifest.json) selects and pins public
repository families before KnowCode is run against them. This document defines the study, not its results.
The acceptance examples used during development are excluded from primary scores; their failures and limits
belong in a separate, clearly labelled section of the results page.

## Corpus and questions

The cohort contains [Jellyfin server](https://github.com/jellyfin/jellyfin) and its
[web client](https://github.com/jellyfin/jellyfin-web), the open-source core of
[Bitwarden server](https://github.com/bitwarden/server) and [clients](https://github.com/bitwarden/clients),
and [Autofac](https://github.com/autofac/Autofac) with its
[dependency-injection integration](https://github.com/autofac/Autofac.Extensions.DependencyInjection).
Bitwarden's pinned license notices identify `bitwarden_license/` as separately licensed; that directory is
excluded from both arms and all labels. Preserve per-file license notices and record any further scope exclusions.
See the pinned [server notice](https://github.com/bitwarden/server/blob/9129efbd1531d5d8472345638efad7ef25a7337a/LICENSE.txt)
and [client notice](https://github.com/bitwarden/clients/blob/abccf1e342dccba06a3cc635cd4c706defa64a32/LICENSE.txt).

Prepare source, without running either arm, using a new directory outside this checkout:

```sh
python3 scripts/prepare-public-eval.py --out /tmp/knowcode-public-eval-source
```

The helper fetches exact commits into local object stores and makes separate read-only source copies. It never
builds or executes project code or initializes submodules. Git export attributes apply; the source receipt records
all omitted tracked paths, explicit exclusions and content hashes. Review those omissions before sampling and
report out-of-corpus dependencies. Existing output data is retained and refused, never refreshed or overwritten.

Verify the source receipt and enumerate every exported file before choosing subjects:

```sh
python3 scripts/census-public-eval.py --source /tmp/knowcode-public-eval-source --out /tmp/knowcode-public-eval-census
```

The census retains every exported file and records its hash, language and review flags. Flags for possible test,
generated, dependency or build files are annotations, not automatic exclusions. In particular, `lib`, `libs` and
`packages` can contain primary application code; do not apply the older bank's directory exclusions blindly.
Review file scope against each repository's project manifests and record any later subject-population exclusions
before drawing questions. Keep the exported files available to both arms even when their declarations are not
eligible sampling subjects, so generated callers and test-only negatives can still be labelled explicitly.

The [subject scope](scope.json) records primary source roots, project-manifest evidence and known external
boundaries for these pins. Apply it to the verified census before drawing subjects:

```sh
python3 scripts/scope-public-eval.py --source /tmp/knowcode-public-eval-source --census /tmp/knowcode-public-eval-census --out /tmp/knowcode-public-eval-scope
```

This step verifies source hashes again and records every file's eligibility and all exclusion reasons. It excludes
test, sample, generated, vendor and build files as sampling subjects, and limits subjects to the listed C#, TypeScript
and JavaScript source roots. It preserves those files as evidence. Eligibility is a source-population definition,
not a check of whether KnowCode supports a declaration or can answer a question. The [sampling plan](sampling.md)
sets task quotas and deterministic candidate ranking. Review candidates in that order for source-extraction errors,
record every rejection and freeze accepted questions before running either arm.

Jellyfin's web SDK dependencies are outside this cohort. Bitwarden's included project manifests reference some
excluded separately licensed projects. Autofac's integration pin references a released package version rather than
the selected core source commit. Labels must disclose these boundaries; the study does not assume that the exported
cohort builds together or contains every production dependency.

These are deliberately selected examples, not a representative sample of all software. Claims must remain within
the tested cohort and task types. Existing language/feature support does not determine which sampled tasks survive:
unsupported patterns, incomplete answers and failures remain in the report.

Sample structural subjects from source populations using a recorded seed and deterministic sorting, before looking
at either arm's answers. Record the census, candidates, accepted questions and every exclusion with a reason.
Separate cross-repository HTTP, cross-repository type/DI/call tracing, multi-hop impact, and single-repository
lookup controls. Do not silently substitute another task when a tool performs poorly. Freeze questions, source pins,
labels, order and run configuration with content hashes before scored runs. Publish the exact sampling procedure.

## Labels

Use the [neutral bank schema](../../scripts/eval/neutral/schema.py) and
[labelling rules](../../scripts/eval/neutral/LABELING.md). Label from pinned source without reading either tool's
output. Each item needs a source file and line or a reproducible source-only command. Separate required, optional,
test-code, negative and out-of-corpus items. Exhaustive labels permit precision and recall; lower-bound labels
permit recall only. Uncertain labels are reported separately. Empty labels must be exhaustively established.
Resolve HTTP paths before labelling their callers. Record assumptions about deployment and generated clients.

When a caller uses an external SDK, labellers may inspect the exact dependency archive pinned by the source
lockfile to resolve its wire contract. Verify the archive's integrity against that lockfile and record package
version, source-member hashes, license notices and the contract evidence. Never execute or install dependency code.
This evidence stays outside both agent views and the sampled source population; it supplies label evidence, not
additional answer files. Disclose that the agents cannot inspect that dependency implementation, and retain any
unresolved version, dispatch, configuration or deployment assumptions. This clarification preceded scored runs.

Independently double-label a seeded subset and publish agreement and adjudications before scoring. A label lacking
independent review is identified as such and cannot support a strong correctness or calibrated-confidence claim.

## Paired arms

Use fresh agent sessions with the same recorded model, effort, question, source view and research-call budget.
The plain arm gets grep, find and read tools. The KnowCode arm gets the same tools plus its MCP surface.
Record the actual schema/profile, binary hash, exact commands, client version and settings. Neither arm can read
labels, previous answers, this repository's evaluation files, unrelated MCP servers, personal memory or plugins.
Neither may edit source or persist notes. Each source view is a separate copy, without `.git`; never hard-link a
writable view to a developer checkout. Respect source privacy metadata before making a view available.

Interleave arms in independently seeded task orders. Run only resources started by the evaluation; do not stop
other sessions or services. Record load and concurrency. Report wall-clock comparisons only under established,
documented quiet conditions. Keep first-run/indexing time, warm query time and agent research time separate.
History/edit questions, if added, require pre-change source and index states so commit subjects cannot reveal truth.

## Draft paired-run preflight

Prepare and inspect a reproducible draft schedule before configuring an agent runner:

```sh
python3 scripts/plan-public-eval.py --source /tmp/knowcode-public-eval-source --questions /tmp/knowcode-public-eval-candidates/questions-draft.jsonl --out /tmp/knowcode-public-eval-plan
```

This re-verifies every exported source file against the pinned receipt, checks question and index pins,
rejects extra question fields that could carry labels, and creates independently seeded task orders for the
plain and KnowCode arms. Sessions alternate arms; every question appears once in each arm. The recorded seed
and SHA-256 ordering rule make the draft independent of input question order. No agents, source code,
indexers, dependency installers or model calls are run. Existing output is refused and retained.

The new output contains only `questions.jsonl`, `order.jsonl` and `plan.json`. Keep it outside source views and
input evidence directories. The plan records source, question, producer and order hashes and explicitly sets
`ready_to_run: false`. It does not copy labels or treat a schedule as a study freeze.

Prepare a new source copy for one session ID from the draft order, outside the plan and original views:

```sh
python3 scripts/prepare-public-eval-session.py --source /tmp/knowcode-public-eval-source --plan /tmp/knowcode-public-eval-plan --session SESSION_ID --out /tmp/knowcode-session-SESSION_ID
```

The helper verifies the plan and source hashes, copies the cohort by value, checks every copied file, and makes
source files and directories read-only. It creates empty `control` and `profile` directories alongside the source;
the client must start in `control`, with a separately verified isolated profile. It does not copy the bank, labels,
private dependency evidence or user settings. Each arm and each session needs its own new copy. Upstream agent
instruction/configuration files remain source data and are listed for explicit auto-loading controls.

`session.json` remains `ready_to_run: false`: read-only copies and an empty profile directory do not enforce
filesystem, tool or network isolation. Client setup, suppression of upstream auto-loading, a separate source-identical
Git/index workspace for KnowCode, independent adjudication and the final freeze are still required. No client or
indexer is launched by this preparation step. Refuse existing session output and retain its evidence.

For the KnowCode arm, prepare separate neutral Git workspaces from the same session source:

```sh
python3 scripts/prepare-public-eval-index.py --source-receipt /tmp/knowcode-public-eval-source/source-receipt.json --session /tmp/knowcode-session-SESSION_ID
```

This creates `control/index` only for a KnowCode session, retaining the complete cohort by value. It verifies
source bytes before and after copying, then initializes fresh repositories with an unborn branch, no refs or
objects, and only the pinned public origin URL as remote metadata. It imports no original history or user Git
templates/settings, invokes no hooks, and makes payload files/directories read-only while allowing Git metadata
maintenance. Agent source views remain unchanged without `.git`. Existing output is refused and retained.
The recorded receipt inside `control/index/neutral-index.json` distinguishes source pins from synthetic Git state.
The current helper refuses source symlinks explicitly rather than omitting them; this cohort contains none.

No clone, fetch, build, KnowCode execution or model call occurs during that preparation. Neutral workspaces are
not graph indexes and remain `ready_to_run: false`. Freeze source labels/configuration first, then build/index with
the adopted audited binary, record indexing separately, and verify the actual MCP surface before scored arms.
The study MCP environment disables personal/system Git configuration and credential prompts; agents remain
prohibited from using Git, and no original commit messages are available through the backend.

An optional `--config PATH` records a draft JSON configuration with exactly `model`, `effort`, `client`,
`client_version`, `research_call_budget`, `knowcode_sha256` and `knowcode_profile`. These are recorded declarations,
not verification of the installed client, binary, advertised schemas or runtime isolation. No model or budget
is selected automatically. Both arms must use the same actual model, effort and research-call budget.

Before either scored arm runs, verify accepted subjects against the sampling population and rejection log,
complete labels and independent adjudication, validate the actual client/binary/schema configuration, construct
separate identical source views with no memory, labels or unrelated integrations accessible, and freeze all those
inputs with the order and runner hashes. The plan's remaining-gate list records this work; it does not establish
quiet conditions or authorize interfering with other sessions. Keep all unsupported and failed tasks in the study.

## Runner interface and offline boundary checks

Preserve the recorded study's bare Claude Code interface. Both arms use Read and the same Bash command allowlist:
`grep`, `find`, `cat`, `sed`, `head`, `tail`, `wc`, `ls`, `sort`, `uniq` and `xargs`. The KnowCode arm additionally
advertises its recorded MCP surface; prohibit `kc_note_add`. Do not replace the plain arm with a new function-tool
interface. State the common research-call budget in the brief and report overruns; the recorded study uses a soft
budget because its bare client cannot enforce a research-call cap.

Use a fresh bare client, strict explicit MCP configuration, no session persistence, an empty working directory
and an isolated profile. Explicitly disable slash-command skills and unintended settings sources: the installed
client's bare-mode help still permits explicit skill resolution. The final runner must verify actual settings and
advertised tools, rather than infer isolation from command names or an empty directory.

The reusable offline macOS boundary in `scripts/eval/macos_sandbox.py` accepts only a prepared session's fixed
source/control/profile roots and an explicit executable list. Paths are profile parameters, never interpolated
policy source. It allows reads under source, explicit executable/runtime reads, and control/profile state access. Source writes,
label reads outside these permitted paths, unlisted executables and network access are denied; state writes stay
under control/profile. It refuses unsupported platforms with no unsandboxed fallback.
Source receipt verification remains a separate required gate; the policy does not establish source integrity.

Run its synthetic native checks on macOS:

```sh
KNOWCODE_TEST_MACOS_SANDBOX=1 python3 -B -m unittest discover -s scripts/eval -p 'test_public_sandbox.py'
```

These tests touch only owned fixtures and a local loopback listener; they start no agent, indexer or model call.
The macOS CI job enables native checks; the portable harness also checks policy construction and refusal cases.
The boundary imports Apple's shipped system runtime profile, records its hashes and must be revalidated on the
actual host. Public CI execution remains unverified until the approved repository exists.

The launch preparer in `scripts/eval/bare_client.py` writes new per-session client settings and an explicit MCP
configuration, without executing either client or server. It disables settings discovery, slash-command skills and
session persistence, grants the same Bash/Read interface to both arms, and records a soft call budget. The plain
arm receives no MCP server. The KnowCode configuration requires a matching binary hash, the complete cohort's
neutral index workspaces, isolated cache/config/data paths and disabled update checks; note creation is denied.
Both prompts expose the full source cohort, including repositories outside the question. Two-hop answers also
record hop assignments separately from their union of source locations. Existing configuration is retained and
refused rather than replaced. These prepared flags do not verify actual runtime permissions or MCP schemas.

The supervisor in `scripts/eval/owned_process.py` starts a fresh session/group for each owned job and preserves its
leader's identity until cleanup. It cleans descendants on normal exit and timeout before reaping that leader,
refuses to signal an unverified group, and preserves transcript/error files. It never scans for other jobs or
uses detached signal timers. Its regression controls start only test-owned processes and check that a separate
control job survives cleanup. The timeout bounds a process; it does not enforce the study's research-call budget.

The parent-side runner in `scripts/eval/session_runner.py` composes these components for one prepared session.
It first validates a `knowcode-public-v1-freeze-1` document with SHA-256 descriptors for every study input and
runner implementation, the client executable, complete labels, the paired schedule and independent blind review
with adjudications. Required input names and implementation paths are defined in `scripts/eval/frozen_study.py`.
Review attestations identify their author; hash checks establish correspondence, not the truth of a label or
the reviewer's independence. A readiness flag cannot replace the underlying evidence.

The runner verifies the copied source and actual client version, prepares and warms the neutral KnowCode index
for its arm, and launches the client through an owned localhost relay. Only the parent holds the provider
credential; the child receives a temporary relay token. The relay forwards to the fixed provider API over TLS.
Provider response IDs must correspond to the recorded client messages, and the actual permission mode must
match the configuration. The runner retains transcripts, relay records, process outcomes and error answers,
then rechecks source, configuration and frozen inputs. Index preparation is outside the agent research interval.
There is no default task deadline or hard research-call cap; an explicitly frozen operational timeout is allowed.

Synthetic composition tests exercise launch refusal, retained failures and relay cleanup. Installed-client tests
exercise streaming, permissions and MCP against scripted local responses. The complete-runner native tests in
`scripts/eval/test_public_runner.py` use a synthetic freeze and source repository in each arm, verify that labels
remain inaccessible, and check index preparation, source answers, transport correspondence and owned cleanup.
These tests make no real provider generation requests and establish no study results. Independent label review,
the final cohort/configuration freeze and actual scored execution remain separate requirements. A successful
client version command alone is metadata evidence.

## Scoring and reporting

Use the existing [neutral scorer](../../scripts/eval/neutral/score.py). Publish per-question source evidence,
answers, tool calls, statuses and score inputs so a reader can reproduce and contest each result. Unsupported,
errored and missing questions remain in the denominator of an additional all-task success measure; report their
counts alongside the scorer's answered-only precision/recall. Do not let abstention inflate an accuracy headline.
Show required-item recall, precision where label completeness permits it, lower-bound results and same-name
negatives. Report empty-answer cases separately.

For confidence, record answer completeness, stale/sampled/withheld disclosures, unresolved or ambiguous links,
and the assumptions behind exact-looking links. Publish correct and incorrect answers by evidence/confidence class;
never treat an `exact` syntactic label as a measured probability. Include failures of disclosure and source mismatch.

Report paired differences by task type and family with seeded confidence intervals and the full distribution.
Keep tool correctness separate from agent behavior. Report research calls, schema/fixed overhead, research context,
total processed tokens, output tokens, refusals and overruns separately. If re-checks are measured, publish the rule
and call sequences; do not count every source read as unnecessary or promise that agents will trust the output.

The results page must show where KnowCode loses, limitations and deviations from this design. No predetermined
improvement is required. A grep comparison supports only the tested plain arm; it establishes nothing about
other systems. No comparative README claim or evaluation number is published before its supporting results.
