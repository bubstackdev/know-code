# Experimental Rust analysis

The first Rust adapter uses the repository-selected **rust-analyzer semantic
engine**, through LSP, and publishes into Know Code's existing worker contract.
It does not invoke rustc to obtain resolved compiler facts, and it is not full
Rust language support. Every compilation is explicitly `partial`, with the
reason retained in coverage. This work begins the planned v0.0.3 language slice;
it does not change the v0.0.1 acceptance requirements.

## Shared boundaries

`providers/rust/worker.mjs` imports the same Protobuf schema/codec as the other
workers. It sends provider identity, compilation scope, retained UTF-8 source,
symbols, occurrences, reports and completion. Source payload hashing and
publication remain in the existing Rust core; this adapter contains no SQLite
or query engine. Node is a worker runtime, not a required embedded query RPC.

Discovered `Cargo.toml` files are evaluated with `cargo metadata --offline
--no-deps --format-version 1`. Member manifests canonicalize to
`workspace_root/Cargo.toml`, and multiple discovered members produce one
workspace compilation. Its ID is `rust:<root-relative-workspace-manifest>`.
Incremental requests for that same manifest replace that same compilation
scope. All workspace members contribute sources, including test targets. The
enclosing Cargo workspace must be inside the registered root. Registering only
a member directory whose Cargo workspace is outside that root fails with an
actionable instruction to register the enclosing workspace/repository root;
the worker does not emit ownership scopes the host cannot later refresh. The
source inventory begins at Cargo target roots and enrolls additional modules only from unique analyzer-resolved module declarations. Orphan/vendor files are excluded. This is still a combined analyzer view rather than exhaustive per-unit compiler ownership. Definitions outside retained first-party source stay external.

Input paths include member/workspace manifests, an existing Cargo lockfile and
ancestor Cargo configuration/toolchain selection files. The coordinator still
owns change reconciliation and scheduling. These are not complete Cargo input
or environment fingerprints: external path dependencies, configuration changes
outside these paths and generated inputs need further work.

## Semantic evidence

The adapter initializes a local rust-analyzer process for each selected Cargo
workspace, waits for its quiescent status and asks for semantic tokens in saved
source snapshots. Identifier-like semantic tokens are resolved with LSP
`textDocument/definition`. Exactly one supported definition is required; missing
or ambiguous results do not become invented exact references. The symbol ID
uses the compilation plus definition path and UTF-8 byte anchor. A location
matching that definition's selection range is a declaration; other locations
are references. This resolves ordinary types, methods, functions, parameters,
locals and cross-file uses in the focused fixture. It is navigation evidence,
not call-graph or runtime-dispatch evidence.

Canonical-definition hover supplies signatures, with documentation and memory
layout disabled so large dependency documentation does not become a signature.
External identities/signatures are published without dependency source
artifacts. External source is read locally when necessary to translate its
definition anchor. IDs currently include local toolchain/dependency paths and
are not ready-made cross-repository package identities.

LSP positions are negotiated as UTF-16 and converted using one offset map per
saved source snapshot to half-open UTF-8 byte ranges. Source text is validated
as UTF-8 and retained with its exact SHA-256. Distinct same-named locals remain
distinct through definition anchors. Moves or preceding edits can change IDs;
refactoring continuity is not implemented here.

## Toolchain and execution policy

Accepted v0.0.3 policy: Cargo build scripts and procedural macros may run under
the existing personal repository trust grant and shared resource limits. They
execute project/dependency code; untrusted workspaces must not run them. Reuse
the common execution-grant path, with generated/configuration provenance and
cancellation. The current 0.0.3 source implements this managed preparation policy; the shipped 0.0.2 adapter and absent-policy legacy direct workers keep scripts/macros disabled.

The worker runs `cargo`, `rustc` and `rust-analyzer` in each selected workspace
directory, allowing rustup's repository selection to apply. It records `rustc
-Vv`, rust-analyzer identity, Node runtime identity and effective analysis
options. Missing tools/components fail with an actionable installation message;
the product never installs a toolchain. Development testing used user-scoped
rust-analyzer and rust-src components matching Rust 1.99.1.

All adapter tool commands and the analyzer subtree force
`RUSTUP_AUTO_INSTALL=0`, retaining the selected toolchain and other environment
settings. An unavailable repository pin therefore produces a prerequisite
error instead of letting a rustup shim download a compiler. Before launching
rust-analyzer, the adapter asks the selected rustc for its sysroot and checks
for the rust-src library manifest/core source; missing sources require separate
component installation. This also prevents the analyzer's missing-source setup
path from installing rust-src. The focused regression uses real rustup shims,
an isolated missing pin and a local download trap: no installation command or
download request is permitted, even if the embedding host enables auto-install.

Managed dependency setup probes the full resolved graph offline, fetching missing dependencies through Cargo only under the personal execution grant. Existing lockfiles are honored with `--locked`; user-selected offline settings and disabled Rustup installation remain authoritative. Shared checkpoints deduplicate setup and preserve failed attempts until explicit refresh. Saved source changes reuse dependency setup while fresh analyzer preparation follows Cargo invalidation. Trusted workers enable build scripts and procedural macros with central target outputs, one Cargo build job and one analyzer thread. The analyzer's own wrapper replacement is disabled so selected repository/environment wrappers survive. No final application or test product is requested. Legacy direct requests remain offline with script/macro execution disabled.

One analyzer lives at a time. Definition requests run in ordered windows of
four; the external-source cache accounts up to 16 MiB of retained payload and
at most 64 entries ([adapter bounds](../providers/rust/navigation.mjs)). Other
requests remain sequential and frames are bounded at 8 MiB; tool commands,
project loading and individual requests time out after 120 seconds. Process
failures/timeouts fail the worker job so the coordinator can preserve prior
publication. Shared coordinator resource controls still apply; these adapter
bounds do not establish actual peak RSS or large-workspace throughput. Owned
source buffers/maps are still materialized for the workspace. Measure that
residency and cold analyzer startup before claiming large-workspace support.

## Honest limits and verification

Accepted v0.0.3 selection policy: use repository-configured target platforms,
or the host when unset, and default features, with workspace target/feature
overrides. Include libraries, binaries and tests with distinct compilation
contexts; do not enumerate every possible feature combination. The current
single-view implementation below does not satisfy that context policy yet.

The current workspace view combines rust-analyzer's default Cargo features and
all-target discovery; it does not produce a separate complete compilation for
every cfg/feature/target combination. `Debug` is a development label, not a
validated Cargo profile mapping; other requested configurations have an
additional coverage warning. Build-script output, procedural macros, generated
sources, standalone Rust files and exhaustive unresolved/diagnostic coverage
remain unsupported. Macro-string or expanded-code references may be absent.
Published analyzer diagnostics are retained when available, but their absence
does not establish a clean rustc build. The adapter always reports this partial
scope, even for a fixture with no errors. No new relationships are emitted.

Run `python3 tests/rust_provider.py` with Node, Cargo, rustc, rust-analyzer and
rust-src available in PATH/its selected toolchain. It checks cross-file method
and function references, Unicode byte coordinates, distinct same-named locals,
test symbols, external identities without dependency source publication, saved
source replacement, unresolved-call behavior, workspace-member deduplication,
out-of-root workspace rejection, build-script non-execution, missing-pin
non-installation, missing rust-src before analyzer startup and actionable
missing prerequisites. The download-trap test needs local loopback permission,
like the coordinator integration's local socket permission. All fixtures run
under `.artifacts/rust-provider/`; the
checked-in fixture is not built.
[Latest focused evidence](../.artifacts/rust-provider/latest.json) records actual
counts and toolchain identity. Host publication/native parity and saved-change
integration belong to the shared integration suite.

The mechanism follows the upstream [rust-analyzer configuration
contract](https://rust-analyzer.github.io/book/configuration.html) and standard
[LSP semantic-token and definition
operations](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/).
The alternative batch SCIP command was considered, but its [CLI explicitly
disclaims subcommand
stability](https://rust-lang.github.io/rust-analyzer/src/rust_analyzer/cli/flags.rs.html)
and would require an additional schema/import boundary. LSP keeps this first
adapter on supported semantic navigation operations; batch extraction remains a
possible measured optimization, not a second storage/query system.

## Real repository: lars-sdk snapshot

Read-only testing used the supplied local `lars-sdk` repository. No Cargo,
rustc, rust-analyzer, Git, build or restore command ran against the original.
Filesystem reads copied 3,934 files (179,968,172 bytes) to
`.artifacts/rust-real/lars-sdk/`, excluding `.git`, build/dependency caches and
all symlinks. The copy includes upstream source/licenses; none of it is Know
Code implementation. SHA-256 plus size, modification/change timestamps, mode
and inode were recorded for 64 original Rust/configuration/instruction files
before copying and after testing; they match. The snapshot's corresponding
files, including `Cargo.lock`, also still match.

Toolchain isolation matters when putting a reference snapshot beneath this
repository: an unpinned snapshot would inherit Know Code's parent
`rust-toolchain.toml`. The original has no repository/ancestor pin or rustup
override and selects the user's default `stable`. The accepted tests explicitly
set `RUSTUP_TOOLCHAIN=stable` and disable automatic installation. That installed
stable compiler is Rust 1.99.1, the same compiler commit as Know Code's pin, but
its rust-analyzer/rust-src components initially were missing. Those matching
components were provisioned user-scoped for development; the product's missing
prerequisite error was retained separately. An exploratory inherited-pin run is
retained but is not the accepted original-selection measurement.

The original Cargo registry cache was copied into an isolated durable test
`CARGO_HOME`. Target, cache and temporary paths all point under
`.artifacts/rust-real/`. The first accepted offline run reported missing cached
`cmake` and returned an honest partial graph. `cargo fetch --locked` then fetched
the snapshot's dependencies into that isolated cache. No build, tests, build
script or procedural macro ran; no target build directory was created. The
provider itself remains offline and does not yet implement this restoration.

| Accepted stable-toolchain run | Before dependency fetch | After locked fetch |
| --- | ---: | ---: |
| Cargo workspace members | 9 | 9 |
| Retained Rust files / bytes | 44 / 516,740 | 44 / 516,740 |
| Symbols | 6,593 | 6,789 |
| Reference/declaration occurrences | 24,731 | 28,252 |
| Worker/harness wall time | 7.21 s | 11.73 s |
| Reported coverage | partial | partial |

These are single worker/harness runs, including cold analyzer startup and
materialized protocol output. They do not measure SQLite publication, native
queries, save latency, peak memory or estate throughput. The dependency graphs
also differ, so the timings are not an optimization comparison. An earlier
exploratory run took about 71 seconds; uncontrolled host/runtime variation
prevents a stable latency claim from this sample.

Validation checked all 28,252 occurrence ranges against retained UTF-8 source
and verified cross-file declaration/reference evidence for `Lars`, `Pending`,
`Calibrator` and `Pending::wait`. Distinct locals include 133 separate `i`
symbols. All 44 published sources belong to the snapshot; the 692 external
symbols have identities/signatures without dependency source artifacts.
Rustdoc links can have semantic token ranges covering link markup, and `Self`
or aliases may spell a resolved symbol differently; reference ranges are exact
source evidence, not a guarantee that every range equals the declaration name.

The prepared run removed the missing-dependency load warning and the earlier
unknown-type diagnostics. It still reports disabled procedural expansion
(including derive/napi uses), plus 395 semantic tokens without one supported
definition. That count is not a compiler-error total or a completeness/recall
percentage. Build-generated cfg/source and feature/target variants remain
uncovered; available checked-in generated bindings are retained as ordinary
source, with generated-origin classification still unfinished. This result is
useful partial navigation, not complete Rust support or proof that the user's
project compiles cleanly.

Retained evidence: [summary](../.artifacts/rust-real/summary.json),
[snapshot provenance](../.artifacts/rust-real/provenance.json),
[toolchain selection](../.artifacts/rust-real/toolchain-provenance.json),
[prepared worker output](../.artifacts/rust-real/result.json),
[pre-fetch output](../.artifacts/rust-real/before-fetch.json),
[dependency fetch log](../.artifacts/rust-real/fetch.stderr.log), and
[original after-test inventory](../.artifacts/rust-real/original-after-test.json).


## 0.0.3 managed preparation checkpoint

The focused owned fixture passed at `a5bd126`: missing dependency restoration
is denied without execution permission; a local Git dependency is restored;
trusted build-script cfg/marker and real proc-macro compilation occur centrally;
repository and environment wrappers reach actual compilation with one Cargo
job; ordinary saves reuse setup; an external macro submodule save recompiles;
missing/malformed/oversized derived plans recover; failed setup needs explicit
retry. Dependency source stays outside published first-party facts. Evidence:
`.artifacts/implementation-0.0.3/wave-01/rust-managed-preparation-01/` and
`.artifacts/rust-managed/1794519920221947000/summary.json`.

Compilation inputs now include resolved manifests, local target/build-script
roots and a bounded watch-only source superset for external path crates. This
is invalidation input, not semantic extraction from filesystem matches. The
fixture proves provider input exposure and manual refreshed-source behavior;
coordinator scheduling still needs assembled acceptance. Arbitrary rerun,
include/generated and environment inputs remain explicit coverage gaps. Setup
cache/config reads and writes have bounds; malformed derived plans are rebuilt
rather than trusted by matching fingerprints alone.

Compiler-backed direct calls, value/type dependencies and ancestor evidence now
use the shared graph fact/query path, with partial capabilities. Stored
Swift/Rust graph and native parity assertions passed before a later unrelated
sibling-invalidation assertion failed; the combined fixture is still under
verification. Exact ordinary/test/binary compiler contexts remain required for
promotion. A finite compiler-artifact-to-separate-analyzer-graph probe passed
five owned no-wrapper contexts; it has not been adopted as the production
context planner. Historical real-corpus measurements above retain their original
scripts-disabled/restoration-external scope.
