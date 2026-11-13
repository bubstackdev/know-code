# Experimental Rust analysis

Rust source analysis remains experimental in shipped v0.0.2. The v0.0.3
integration branch now captures actual Cargo/rustc compilation contexts and
uses rust-analyzer for semantic extraction within each selected context. All
Rust graph capabilities remain explicitly partial. Rust as Know Code's core
implementation language does not establish Rust source-language support.

| State | Evidence and boundary |
| --- | --- |
| Shipped v0.0.2 | Experimental adapter; no v0.0.3 language-release claim. |
| Integrated at `8155a11` | Managed compiler-captured contexts, selected-root ownership, direct calls/dependencies, native parity and policy backfill. |
| Macro source integrated at `7e10a53` (from `dbf6e54`) | Captured dylib replay, synchronous diagnostic barrier, environment-delta/cache proof and conservative macro-origin omissions. Snapshot07 passes all 14 finite macro assertions; I/O and actual Cargo collection checks pass separately. Matching assembled/native checks pass; installed and release checks remain pending. |

## One provider contract and one core

The Node worker shares `providers/shared/protocol.mjs` and the versioned bounded
Protobuf contract with other providers. It emits compilation identities, saved
UTF-8 source, symbols, occurrences, relationships and analysis reports. The Rust
core owns storage, atomic publication, updates and bounded queries; the adapter
contains no SQLite or graph-query engine. Native .NET/Node queries stay
in-process. Rust-specific package/compiler planning stays in narrow adapter
modules rather than another scheduler or validation framework.

Cargo manifests canonicalize to the evaluated workspace manifest, deduplicating
member requests. A workspace outside the registered root is rejected with an
actionable registration error. Standalone Rust files are not supported. Managed
requests carry the shared Rust analysis policy. Historical direct-worker
requests without policy retain a combined analyzer view for focused legacy
fixtures; that view is never relabeled as an exact captured context.

## Managed compiler contexts and ownership

The coordinator resolves repository/default target platforms, selected features
and the default-features setting. For each selection the adapter performs
`cargo check --offline --workspace --all-targets --keep-going --jobs 1` with
bounded JSON output and its private compiler wrapper. Actual ordinary/test
library, binary and integration contexts remain distinct. It does not enumerate
every feature combination or select a Cargo profile from the workspace's C#
configuration label. Non-`Debug` labels carry the current unmapped-profile
warning.

A context requires successful compiler status, an exact Cargo-output/rustc-output
join, emitted dep-info membership, matching pre-compilation source bytes,
content-bound extern artifacts and the actual compiler/cfg/sysroot identity.
Unknown arguments, wrappers, escaped dep-info forms or incompatible reachable
cfg selections remain unavailable. Extern artifact provenance joins crates;
package IDs remain opaque. Required metadata is prepared, while unexpected
application/test linking or code generation is refused before execution.
Build-script/proc-macro tool roots may emit the tools needed for preparation.

Each selected unit owns only its consumed first-party Rust source. Dependency
crates, including sibling workspace units, are lookup-only in that graph.
Dependency sources are not published as owned artifacts, and no cross-unit body
link is inferred from names or paths. Identical source storage across contexts
remains core-owned. Known saved inputs are rechecked before extraction; changed
compiler-consumed bytes fail the job rather than acquire stale ranges.
Build-script OUT_DIR membership labels captured generated source where known;
this is not exhaustive expansion-origin mapping.

The collector reuses content-bound metadata proof, not a resident compiler
workspace. Bounded package snapshots cover ordinary inputs; one recapture may
discover previously unrecorded consumed files. Cleanup removes only proven
regular outputs under the owned target/build directories, refusing foreign,
state or symlink paths. `--keep-going` permits healthy contexts after a failing
sibling; failed units cannot borrow prior successful proof. Aggregate compiler
diagnostics remain on workspace-summary/unavailable rows rather than being
copied as failures into every healthy unit.

Integrated rows stamp `optionsJson.rustContextPolicy=2`. Earlier combined views
and policy-1 selected/unavailable rows require ordinary bounded backfill for the
macro policy; current policy-2 unavailable rows avoid retry loops. The existing common
readiness/backfill path requests bounded reanalysis for older combined-view
rows, including unchanged repositories after restart. Current unavailable rows
carry the marker too, avoiding expensive retry loops.

## Semantic and graph evidence

One analyzer at a time receives the selected crate graph, effective cfg and
compiler-selected sysroot. Saved semantic tokens supply request positions;
exactly one supported `textDocument/definition` selects a symbol. Its identity
combines the compilation, definition path and UTF-8 anchor. Authored declaration
kind/range evidence is required in managed contexts; unproved expansion anchors
are omitted. Distinct locals and parameters remain queryable. Moves/preceding
edits can change IDs; refactoring continuity is not implemented here.

The analyzer syntax extension supplies grammar roles and source ownership;
outgoing-call evidence is additionally required before emitting a call.
References remain distinct from calls. Explicit source-module and
constant/static initializer nodes preserve execution ownership. Deferred
closures/async bodies, trait/dynamic dispatch, broad implicit calls and
cross-unit links remain partial; no runtime execution claim follows from a
compiler-bound edge. Shared Rust callers/callees/impact operators consume these
ordinary relationships.

Canonical-definition hover supplies display signatures; documentation/memory
layout are disabled. Hover text is not parsed to establish semantic identity.
External identities/signatures may be retained without dependency source
artifacts. Their local toolchain/package paths are not a finished estate-wide
package identity. LSP UTF-16 positions and syntax UTF-8 byte columns are converted
against the exact saved UTF-8 source/SHA-256, including CRLF normalization.

## Trust, restoration and toolchains

Managed compiler/dependency execution requires the existing separate personal
repository trust grant. Opening/registering does not grant trust. Cargo build
scripts and procedural macros are covered by that accepted grant and shared
resource limits; no additional Rust-only grant is introduced. Package-manager
lifecycle-script grants remain a separate policy.

Managed preparation first evaluates offline resolved metadata. Missing packages
can trigger Cargo fetch through the common preparation/checkpoint path,
respecting an existing lockfile and repository Cargo selections. Cargo owns its
credentials/cache. Source-only saves reuse successful setup; failed setup needs
changed dependency inputs or explicit refresh to retry. Fetch does not build
products or execute build scripts/macros. Compiler-context preparation itself
may execute those trusted compiler tools.

All ordinary analysis forces Cargo offline and `RUSTUP_AUTO_INSTALL=0`; only the
explicit dependency-fetch phase drops Know Code's forced offline value, while
preserving user-selected offline settings. Node, Cargo/rustc, rust-analyzer and
matching rust-src are separate prerequisites, never auto-installed by the
product. Effective compiler identities stay separate from the worker runtime.
The candidate macro path validates rust-src against Cargo's actual captured
compiler rather than an unrelated PATH rustc. Unsupported wrappers/custom
compiler equivalence remain explicit gaps.

## Procedural-macro replay in the integration branch

This section describes integrated development source, not shipped support.
The candidate supplies an actual compiler-emitted macro dylib only when exact
Cargo provenance and every consumer's extern-content hash agree. It checks the
matching compiler/sysroot and runs one analyzer/macro process at a time. It does
not rerun Cargo build scripts inside the analyzer or silently disable failed
macro replay.

Readiness requires healthy analyzer status and full synchronous document
pull-diagnostics over the exact owned saved text. A healthy status notification
alone cannot prove that a dylib loaded. A loader/error report yields explicit
partial/unavailable coverage with no macro-disabled fallback facts. Function
and method identities additionally require matching typed canonical
call-hierarchy descriptors at use and declaration, including the authored name;
alias spellings do not substitute for declaration names.

Noncallable names lack equivalent universal typed canonical proof. The newest
candidate therefore omits them beneath attributed syntax or unproved module
ancestry, including fields/locals/parameters beneath transformed owners and
out-of-line module descendants. This is deliberately conservative for built-in
or inert attributes too. Proven callable identities remain available; plain
unattributed type/field identities retain ordinary authored proof. Lookup-only
non-root module ancestry is also an explicit omission boundary, not guessed
from filenames. The corrected module ancestry, transformed-type/field
omissions and ordinary nested-module preservation pass the owned snapshot07
fixture; broader expansion-origin coverage remains partial.

Cargo-injected environment sets/overrides and required dep-info values are
retained for replay, with a bounded hashed parent baseline for warm proof.
Unchanged arbitrary ambient values are not dumped into index/cache records.
Warm reuse binds the entire hashed parent baseline, detecting changed ambient
inputs without retaining their raw values. Public per-crate replay cannot
represent inherited-variable deletion, so those macro contexts are refused.
Wrapper/jobserver/capture controls are excluded. Arbitrary macro runtime file
reads, environment-dependent/nondeterministic expansion and expanded-source
identities remain explicitly uncertified; this is not proof of exhaustive
compiler/analyzer equivalence.

## Bounds and actual validation

The integrated macro/backfill candidate at `2009f5f` has matching Debug producer
`a76e4dfb22004262906f6620102a48de`. The focused managed/context catalog passes
at test revision `9ead59b`, including actual restored dependencies, trusted
build scripts, macro edits, explicit wrapper refusal and the 14 macro-origin
checks. Evidence: `.artifacts/implementation-0.0.3/wave-01/assembled-rust-providers-07/`.
All 20 publication tests, formatting and strict workspace Clippy pass for the
policy-2 backfill change. The preceding catalog failures retain obsolete
wrapper and policy-1 fixture assumptions; neither is relabeled successful.
Matching four-language native integration also passes all 21 assertions
(`wave-01/assembled-language-contexts-07/`), including policy-2 normal/test
coverage, stored calls/impact, saved changes and pinned source/empty-generation
behavior. This remains Debug integration evidence; optimized, installed, real
corpus and release certification are separate.

Definition requests use ordered windows of four. The external-source cache
accounts at most 16 MiB of retained payload and 64 entries. Lookup-only files
above 64 KiB omit syntax-origin proof. A direct lookup-only syntax-presentation
RPC failure can likewise omit proof; owned syntax, source mismatch and transport
failures remain fatal. Origin/canonical caches and module-uncertainty traversal
are bounded. Worker frames are bounded at 8 MiB; command/project/request
timeouts are 120 seconds. Shared coordinator limits still apply. These bounds
are not true peak-RSS, large-corpus or responsiveness guarantees; owned source
buffers/maps are still materialized for the selected unit.

- Integrated source: the context catalog passed at `ab992a6` and selected-worker
  fixture at `4ea97fa`. Exact receipts are under
  `.artifacts/implementation-0.0.3/wave-01/rust-contexts-catalog-03/` and
  `rust-context-worker-01/`.
- Matching Debug producer `9aa4dcffb1aa441dabd4545b65c67032` at `8155a11`
  passed `assembled-language-contexts-06` (21 assertions, about 109 seconds),
  including Rust normal/test coverage, shared source artifacts, stored graph
  and native parity, saved changes, empty publication and pinned reads. This
  is not optimized/installed/release or real-corpus performance certification.
- Immutable snapshot07 at `dbf6e54`, subsequently integrated, passes all 14 macro
  assertions: both captured library contexts, forced Cargo environment selecting
  the correct valid binding, ordinary/attribute-preserved and aliased calls,
  same-kind renamed function/type/field omissions, attributed module ancestry,
  plain nested-module identities, generated-identity omissions, lookup-only
  dependencies, actual captured compiler selection, synchronous diagnostics,
  optional syntax-presentation failure and actual dylib-loader failure with no
  fallback facts. Exact receipt:
  `.artifacts/implementation-0.0.3/rust-macro-source-07/source/.artifacts/rust-macro-worker/1794579719177751000/summary.json`.
  Source/dependency manifests remained unchanged. The root process receipt at
  `rust-macro-worker-snapshot-07-processes.json` records 14.68 seconds and
  838,096 KiB maximum sampled aggregate descendant RSS. That approximately
  818 MiB sum is a sampled lower bound, not unique memory, a true peak, a hard
  cap or a repeatable performance target; cache warmth was not controlled for
  timing comparisons.
- The updated pure I/O/environment guards and actual Cargo warm-cache/
  failure checks passed separately in
  `wave-01/{rust-context-io-snapshot-06,rust-context-collection-snapshot-06}/`.
  The original macro snapshot06 failure remains retained: it exposed `MOD`
  instead of the analyzer grammar
  [`MODULE`](https://github.com/rust-lang/rust-analyzer/blob/master/crates/syntax/src/ast/generated/nodes.rs)
  in owner/declaration matching. `dbf6e54` corrects those three literals without
  weakening the fixture; snapshot07 records actual grammar kinds as well.
  Earlier snapshot05 passed its narrower 11-assertion fixture. None of these
  worker-only receipts certifies the assembled candidate's native, installed,
  real-corpus, optimized or release behavior.

Focused checks are `tests/rust_context_io.mjs`, `rust_contexts.mjs`,
`rust_context_collection.mjs`, `rust_context_worker.py`, `rust_macro_worker.py`
and the existing `rust_provider.py`/four-language native integration. Choose the
relevant cases and matching source/artifacts through the shared validation
owner; do not run a full suite for every adapter edit. Preserve failed attempts.
The historical direct-worker tests still verify scripts-off behavior; their
success must not be relabeled as managed build-script/macro qualification.

## Historical combined-view corpus: lars-sdk snapshot

This earlier run used the scripts-off, combined-view adapter. It does not
qualify the current captured-context or candidate macro-replay paths.

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
adapter at that checkpoint had no automatic Cargo restoration; current managed
preparation is described above.

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
