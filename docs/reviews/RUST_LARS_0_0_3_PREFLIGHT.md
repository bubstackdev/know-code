# Lars Rust 0.0.3 preflight

Source/read-only audit, 2026-11-13, against main `a53f99a`. No Cargo,
compiler, analyzer, repository code, trust change or original mutation was run.

## Retained corpus and permission boundary

The identified owned Lars corpus is `.artifacts/rust-real/lars-sdk/`.
Its `provenance.json` records the original
`/Users/zakkeown/Code/bubstackdev/lars-sdk`, 3,934 copied files / 179,968,172
bytes, no followed symlinks, and exclusions including `.git`, `target` and
build/dependency caches. `original-before.json`, `original-after-copy.json`
and `original-after-test.json` retain the historical 64-file Rust/config
hash/stat checks. Preserve this snapshot and its result files unchanged.
The pruned retained-artifact census found no other directory named `lars-sdk`.

The historical `request.json` contains no `analysisPolicy`; its direct worker
was scripts/macros-off. It is not a current personal-trust receipt. The explicit
`research-trust-approval-01.json` covers only the two named Atelier 02/03
snapshots and their personal-data roots, not Lars. No applicable Lars grant was
established by this audit. A fresh owned full copy and empty central
personal-data sibling make a new, narrowly scoped personal grant reviewable;
opening it alone cannot grant execution. Do not write grants from a test script
or reuse unrelated Swift fixture grants.

## Fresh reviewable copy prepared

The new main-checkout artifact
`.artifacts/implementation-0.0.3/lars-production-full-01/snapshot` now retains
3,934 regular files / 179,968,172 bytes, root plus all nine member manifests,
vendored native inputs and test resources, with zero retained symlinks and no
source rewrites. Its sibling `personal-data` was created empty/untrusted.
Full source SHA256/stat/membership before/after checks match; copied file hashes,
mode and mtime match. Exact exclusions and selected copy-process HOME/tool/env
inputs are recorded; other ambient values are hashed. No compiler/restore/index
or external target-artifact copying occurred. Historical evidence is untouched.

The preparation manifest is
`.artifacts/implementation-0.0.3/lars-production-full-01/manifest.json`, SHA256
`9491a34a6da6d6fb9cb59d8ce3a12148d190238ef27c16659ed0599895e503d9`.
It binds the original-before/after, copied census and environment receipts.
This prepares a concrete trust scope; it is not a grant or a qualification run.

## What the existing results prove

`.artifacts/rust-real/summary.json` records a successful **partial combined
rust-analyzer workspace view**, not selected compiler contexts. After an isolated
locked dependency fetch it emitted 6,789 symbols, 28,252 occurrences and 44
source artifacts / 516,740 bytes in 11.73 seconds, with scripts/proc macros
disabled. Exact saved-source bytes and cross-file references to Lars, Pending,
Calibrator and wait were checked. The retained `target` directory remains
absent. This cannot qualify captured cfgs, actual macros, build-script inputs,
normal/test separation, current storage/native graph, or current performance.

Current finite owned evidence is documented in `docs/RUST_PROVIDER.md`:
`wave-01/assembled-rust-providers-07/` covers the managed/context catalog;
`assembled-language-contexts-07/` covers 21 matching Debug native integration
assertions; immutable macro snapshot07 covers 14 origin/env/loader checks.
Those prove specific fixtures, not Lars or arbitrary compiler flags/macros.
Preserve the earlier failed snapshots and their narrower evidence boundaries.

## Source and environment risks for a first captured pass

- Lars has nine workspace members, local member dependencies, edition 2024,
  rust-version 1.85, and a checked-in Cargo.lock. Neither the current original
  nor retained copy has a repository `rust-toolchain*` or `.cargo/config*`.
  An artifact nested inside Know Code would inherit Know Code's toolchain pin
  unless the original effective selection is explicitly retained. Historical
  execution used `RUSTUP_TOOLCHAIN=stable`, rustc/rust-analyzer 1.99.1, and
  `RUSTUP_AUTO_INSTALL=0`; record current selected identities rather than
  treating that old version as newly measured.
- `crates/lars-llama-sys/build.rs` runs CMake over the vendored llama.cpp tree,
  compiles vision.cpp, enables Metal on Apple targets, and emits link paths and
  `LARS_LLAMA_CPP_VERSION`. The tree's CMakeLists.txt and 40-character revision
  stamp exist in the retained source. This is real C/C++/Metal work even under
  Cargo check; the old scripts-off timing says nothing about its cost. CMake,
  selected Xcode/CLT/C++ tools and SDK must be available. Do not omit this member
  or change LARS_LLAMA_NATIVE to manufacture a successful corpus.
- `crates/lars-node/build.rs` invokes napi_build; serde, napi and the local
  `rust/derive` crate exercise actual procedural macros. Keep the existing
  exact captured dylib/toolchain/env proof and conservative authored-anchor
  omissions; a successful build does not certify expanded declaration identity.
- Checked-in `bindings.rs` is included by lars-llama-sys; image tests use
  include_bytes! on `fixtures/images/{red,blue}.png`. Keep non-Rust files and
  native source/resources in the full copy. Cargo check compiles test contexts;
  it must not run test bodies, model downloads, runtime pack loading or c_smoke's
  nested Cargo invocation.
- Runtime HOME/LARS_PACK_DIR/LARS_PACK_URL/LARS_TEST_* reads are distinct from
  compile-time inputs. **The external prebuilt liblars.a requirement is the
  Swift Atelier integration boundary**, whose Package.swift uses LARS_SDK_PATH
  or HOME/Code/bubstackdev/lars-sdk and LARS_PATH or target/atelier-host. It is
  not a demonstrated Rust Cargo prerequisite: the Rust workspace builds its
  native dependency from vendored source. Do not copy a stale target library
  into the Rust source snapshot or rewrite these external Swift paths.
- At audit time CARGO_HOME, CARGO_TARGET_DIR, RUSTUP_HOME, RUSTUP_TOOLCHAIN,
  RUSTC, all four supported wrapper-selector env names, CARGO_BUILD_TARGET,
  LARS_LLAMA_NATIVE, CC, CXX, CMAKE, SDKROOT and DEVELOPER_DIR were unset in this
  agent; HOME was /Users/zakkeown. User ~/.cargo/config{,.toml} were absent.
  This is a preflight observation, not the future owner's effective environment.
  Record allowed tool/path variables and hashes of other ambient inputs without
  dumping credentials. Preserve real HOME and selected tools. Keep an isolated
  Cargo cache as an explicit harness input, not a hidden product default.

## Minimum useful current qualification

1. Freeze a full owned source copy with file hashes/stats, exclusions and an
   original-before/after check. Retain all nine members, vendored native sources,
   resources and lockfiles. Use a fresh central personal-data/compiler-cache
   directory, fresh index, and the actual user-granted trust scope. Reuse the
   shared preparation/refresh/coordinator path, matching producer and shared
   resource/process sampler. Do not execute against the original or historical
   snapshot, install tools, grant permission, or alter the live coordinator.
2. Reuse the request/framing and validation patterns in
   `tests/rust_context_worker.py`, `tests/rust_managed.py`,
   `tests/rust_macro_worker.py` and `tests/worker_harness.mjs`. A small corpus
   request should select the root Cargo.toml with analysisPolicy v1 / Rust
   executionAllowed from the granted boundary, fresh cacheDirectory, repository
   defaults and no invented feature/target overrides. Retain frames, command
   diagnostics, preparation checkpoints, exact source manifests and sampled
   process/resource evidence. The historical run.py/validate.py require changes
   to assumptions and **must not be rerun in place** (one combined view and
   scripts-off assertions are obsolete for this purpose).
3. Require policy-2 actual selected-unit identities/options, exact cfg/profile/
   target separation and stable source coordinates; inventory every expected
   member and normal/test/binary context from actual Cargo output. Report failed
   units explicitly and publish healthy siblings as partial. Check at least
   one authored direct call/callee and impact path within a proven root unit,
   plus a lookup-only dependency boundary with no guessed cross-unit body link.
   Reuse current core/native query assertions for the stored graph rather than
   treating raw worker facts as full production acceptance.
4. Repeat unchanged to demonstrate preparation/capture reuse. On the owned copy
   only, make one reversible saved-source edit and verify changed input proof,
   fresh graph, stable logical context IDs and dependency-setup reuse; retain
   both snapshots and restore the owned source bytes. Verify ordinary compiler
   outputs remain central and original before/after hashes match.

Failure is useful evidence: preserve an explicit unavailable/resource-limited
result if native preparation exceeds current limits. Existing guards refuse
unsupported wrappers/includes/quoted-key configuration and escaped dep-info;
input/extern/artifact proof, macro loader failures and shared time/memory/disk
budgets still apply. Do not widen policies, skip members, silently swap tools,
raise limits or add a new validation framework to turn this corpus green.
No current exact-context Lars run or installed/release/performance result is
claimed here.

## First current production run: measured failure and narrow correction

Root-owned `lars-production-full-01/production-default-01/` completed in 109.44s
under the unchanged 2 CPU / 2 GiB defaults. The coordinator published generation
2 with 12 contexts, including healthy C#/TypeScript projects and an explicit Rust
unavailable summary; no Rust graph qualified. Owned source checks passed and the
owned coordinator stopped. This is a failed Rust qualification, not success from
the healthy siblings. The unavailable Moedex.Lars NuGet package is a separate
repository dependency problem.

Read-only follow-up retained `failure-analysis-readonly.json` beside that run.
Its exact `.fingerprint/*/output-*` files identify eight library codegen refusals:
`dtor-30fe02138d717190`, `find-msvc-tools-3b2cebe2cc9a9ec4`,
`napi-build-d618bda998d72d86`, `semver-1aa84e9ae4fd67c7`,
`shlex-655b58d0674bbf02`, `unicode-ident-61ce588b7457064a`,
`unicode-segmentation-5e808ecb7d5a4ff3`, `version_check-b27f711fdcf7628f`.
The retained dependency-plan's typed resolve graph places these on actual
build-script/proc-macro dependency chains: cc→find-msvc-tools/shlex,
generic-array build→version_check, napi/lars-node build→napi-build,
derive→proc-macro2/syn→unicode-ident, napi-derive→convert_case→unicode-segmentation,
→ctor→dtor and →napi-derive-backend→semver. They are ordinary library dependencies
needed to build executable tooling, not requested final applications.

`cargo-contexts.mjs` supplies only direct custom-build/proc-macro source roots to
`capture.mjs`, whose emit guard permits link only for those roots. A narrow fix
can admit intermediate **lib/rlib** codegen for typed tooling-dependency roots,
while continuing to reject application/test executable, cdylib/staticlib and
other unsupported outputs. Retain all exact source/extern-before, emitted-output,
compiler/cfg/environment and Cargo artifact joins; a permission whitelist is not
semantic producer evidence. Failed raw command records are scratch-cleaned by
`collectCargoContexts` finally, so these fingerprint operands do not recover the
deleted complete argv. Preserve that evidence limit; do not invent measured args.

The ninth fingerprint refusal is `moedex-lars-derive-7f9bf401d0d2d45e`'s **test**
library: `externArguments` currently allows bare sysroot proc_macro only for an
explicit proc-macro crate type. A narrow metadata/test extension must preserve
its no general crate/all search path, no explicit same-name alias and exact
compiler/sysroot-source proof; never guess a user artifact by crate name.

The surviving local derive unit is separately proven status 0 with
`--crate-type proc-macro --emit=dep-info,metadata`, a Cargo `.rmeta` filename and
only dep-info/metadata artifact messages. Its no-dylib warning is honest: it is
not an observed loader failure. It must never become a fabricated executable
macro. An own-root metadata-only context may remain explicitly unsupported;
every reachable **macro dependency** still needs its unique actual emitted
dylib and exact consumer hash join. No correction or runtime rerun is claimed by
this read-only analysis.
