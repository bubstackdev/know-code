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
confirmation; the runner does not infer quiet conditions or manipulate other processes. Environment labels are
operator declarations, with platform and architecture checked for the macOS and Linux labels.

## Clean-machine gate

Isolated-profile success does not complete this gate. Repeat on a fresh Apple-silicon macOS user account when the
machine is quiet, and in a clean Linux x86_64 container. Record whether Linux runs natively or under emulation.
Do not restart shared services or stop other sessions to obtain these conditions.

In each clean environment, additionally verify the default installation path and shell profile behavior:

1. Start without a KnowCode executable, cache, configuration, notes, or client registration. Install Claude Code.
2. Install with the single generated shell command and no path/configuration overrides. Before the public release,
   use the reviewed local installer and artifact mirror; record that this does not verify public download hosting.
3. Start a new shell as instructed by the installer. `knowcode --version` must resolve through the default PATH.
4. In the fresh clone, run `knowcode setup claude`. Start a new Claude session and invoke `kc_context` using the
   manifest's symbol. Do not run an index command or edit configuration.
5. Save the first answer, source evidence, incomplete disclosures, time to a correct complete answer, machine and
   load, binary checksum, exact installation command, and repository commit. Repeat with the other case.
6. Verify uninstall removes the registration and disposable indexes and preserves unrelated client settings.

The isolated runner can provide the reproducible protocol check within those environments using the matching
`--environment` label. Its report always records `default_paths_validated: false`; attach separate evidence for
the default-path steps above. Do not mark clean-machine acceptance complete from that report alone.

Public download hosting and public CI require separate verification after the audited snapshot is approved and
published. Keep comparative claims and evaluation numbers out of public materials until their supporting public
evaluation is published.
