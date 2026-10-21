# Public-repository first-run acceptance

The pinned cases in [public-repos.json](../scripts/acceptance/public-repos.json) check C# HTTP handlers in
dotnet/eShop and the archived eShopOnContainers `dev` branch, plus a small TypeScript library, mitt. The manifest records the question, source file and declaration
line, expected source details, and, for eShop, the HTTP caller in another project. These are acceptance cases,
not a comparative evaluation or a measurement of agent behavior.

The modern eShop case exercises minimal-API `MapGet`/`MapGroup` registrations through a helper method; the
controller-based case exercises attribute routes. Their source lookup and cross-project HTTP checks are separate;
successful lookup must not hide a missing HTTP caller.

## Isolated installation and protocol check

Build the native preview with `bash scripts/package-native.sh`. Install Claude Code before running these checks.
Clone each public repository at the manifest's exact commit; the runner requires a clean checkout. Then run:

```sh
python3 scripts/check-public-first-run.py eshop --repo /path/to/eShop --output /tmp/knowcode-eshop-first-run.json
python3 scripts/check-public-first-run.py eshop-controllers --repo /path/to/eShopOnContainers --output /tmp/knowcode-controllers-first-run.json
python3 scripts/check-public-first-run.py mitt --repo /path/to/mitt --output /tmp/knowcode-mitt-first-run.json
```

The runner installs the native archive using its generated shell installer, runs `knowcode setup claude`, verifies
that Claude enabled the real bundled plugin, and launches the MCP server from that installed plugin's configuration.
No manual indexing or client configuration editing occurs. `kc_context` must return the expected source and
declaration; the C# cases must also return the caller in the other project. A pending index must be disclosed as stale,
and the complete answer must arrive within five minutes. Complete here means converged: no `incomplete` entry is
`stale`, and every entry left has a reason the case allows (`allowed_incomplete` in the manifest, empty by default).
Any other reason left (`failed`, `deadline`, `budget`, `missing_repo`, `withheld`, `policy`, or one the case doesn't
allow) fails the case. The reasons left are recorded in the report (`remaining_incomplete`). Each response and check
is saved, including failures.

The runner creates isolated configuration, data, cache, installer receipt, and Claude profile directories.
It never changes `HOME`. It closes only its own MCP process and removes its own temporary installation.
Update checks are disabled so external release availability does not affect the measurement. Reports stay outside
the source snapshot: local paths and unreviewed timing results must not accidentally become public claims.

Use `--machine-notes` to record competing work or architecture emulation. `--quiet-confirmed` records an operator's
confirmation; the runner does not infer quiet conditions or manipulate other processes. Environment labels
(`fresh-macos-account`, `clean-linux-x86_64-container`, `clean-linux-arm64`) are operator declarations, with platform
and architecture checked.

## Clean-machine gate

Isolated-profile success does not complete this gate. Repeat on a fresh Apple-silicon macOS user account when the
machine is quiet, and on clean Linux x86_64 (a container is fine) and, when available, Linux arm64. Record whether
Linux runs natively or under emulation. Do not restart shared services or stop other sessions to obtain these
conditions.

In each clean environment, install Claude Code and sign in, then run as that user, in a terminal:

```sh
bash scripts/acceptance/clean-machine.sh --commit <sha> [--artifacts <dir>]
```

[clean-machine.sh](../scripts/acceptance/clean-machine.sh) installs into the real home directory: run it only on a
machine or account set aside for this. It asks whether the machine is quiet and for notes (emulation, competing
work), then:

1. Records the machine (OS, architecture, CPU, memory, load, client version) and checks it is clean: no `knowcode`
   on PATH, no KnowCode cache, configuration or data directory, no installer directory (`~/.config/knowcode`), no
   `KNOWCODE_*` overrides, and no KnowCode plugin, marketplace or settings entry in Claude Code.
2. Clones KnowCode at the given commit and each case in the manifest at its pinned commit, and runs
   `scripts/check-public-first-run.py` per case with the environment label for the platform and the operator's notes.
3. Installs with the single shell command and no path or configuration overrides: from a local artifact mirror
   (`KNOWCODE_DOWNLOAD_URL=file://…`) with `--artifacts`, which does not verify public download hosting, otherwise
   from the public release. The report records which, the exact command and the binary checksum.
4. Records the installer's shell-profile changes and whether `~/.local/bin` was already on PATH. When it was not,
   the installer must have added it; `--hide-local-bin` runs the installer without it to cover that case on a
   machine whose PATH already has it. A new login shell must then resolve `knowcode --version`. New shells are
   started with `-l` and stdin from `/dev/null`, never `-i`, and source the rc file an interactive shell reads.
5. Runs `knowcode setup claude` in each case clone. No index command and no configuration edit.
6. Asks the operator for two real Claude Code sessions per case, each in a new terminal: unprompted (the manifest's
   question) and named ("Use the KnowCode kc_context tool to answer: …"). From the client's transcripts it reports,
   per session, the prompt, `used_knowcode`, KnowCode calls, other tool calls and seconds to the final answer. These
   numbers are recorded, not gated. `report/knowcode-use.json` holds the same counts in machine-readable form: one row
   per case and session with KnowCode calls by tool, and a total for the unprompted sessions.
7. Snapshots Claude Code's settings right before uninstall, runs `knowcode uninstall --delete-config
   --delete-notes`, and checks the exit code, that the plugin and marketplace are gone, that no index and no KnowCode
   cache, configuration or data directory is left, and
   that settings are unchanged entry by entry except KnowCode's own entries (`enabledPlugins` and
   `extraKnownMarketplaces` entries for `knowcode-local`); no other entry and no whole key may go.
8. Pauses while the operator follows uninstall's printed removal steps, then checks `knowcode` is gone, no profile
   line remains, and a new login shell starts without errors it did not print before installation.

Every step is recorded in `report/steps.tsv` and `report/summary.md` under the work directory (default
`~/knowcode-acceptance`), archived as `clean-machine-report.tar.gz`. The script exits non-zero when a gated step
failed. `--dry-run` runs only the machine record and the read-only clean checks and prints the plan; `--self-test`
runs the tests of the transcript parser and the settings comparison.

The isolated runner's report always records `default_paths_validated: false`; the default-path steps above are the
script's own. Do not mark clean-machine acceptance complete from the runner's report alone.

Public download hosting and public CI require separate verification after the audited snapshot is approved and
published. Keep comparative claims and evaluation numbers out of public materials until their supporting public
evaluation is published.

## Tuning unprompted use

[adoption_tune.py](../scripts/acceptance/adoption_tune.py) is a small loop for tuning the server instructions, tool
descriptions and skill descriptions against unprompted use. It is separate from the frozen public study: its
questions ([adoption-questions.jsonl](../scripts/acceptance/adoption-questions.jsonl)) are not the study's and use
other public repos. Each session is a headless `claude --bare --print` run in a clone, with KnowCode served but not
named, in an isolated home, client configuration and KnowCode cache; it counts KnowCode calls and other tool calls.
Two configurations: the MCP server alone (`mcp`), and the server with the plugin's skills (`plugin`). Bare mode skips
hooks and offers no tool search, so the Bash hint hook and the always-loaded set play no part; `--full-client` turns
bare mode off, so both do, as in an interactive session. The API key is read from `ANTHROPIC_API_KEY`; each session's
spend goes to a ledger, and no session starts past `--budget-usd`.

```sh
python3 scripts/acceptance/adoption_tune.py run --repos <clones> --knowcode <binary> --work <dir> --label before --budget-usd 5
python3 scripts/acceptance/adoption_tune.py summary --work <dir>
```

Its counts are a small sample for tuning, not a result.
