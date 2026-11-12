# Validation frequency

Use focused checks while developing, the required public TS/C# baseline for a
release checkpoint, and additional existing matrices when the reviewed change
affects their contracts. Keep the cases; reduce unrelated repetition. One owner
runs heavy builds/tests, with isolated fixture data and unchanged real HOME.

```sh
bash scripts/test.sh                       # debug quick; no native build needed
bash scripts/test.sh --list                # exact suite/check IDs, no execution
bash scripts/test.sh jsonrpc build-record  # explicit source-only checks
bash scripts/test.sh debug rust cursors    # build matching debug artifacts first for cursors
bash scripts/build.sh release
bash scripts/test.sh release core          # required public release baseline
bash scripts/test.sh release project-links dispatch  # add relevant matrices
bash scripts/test.sh extended              # every retained extra, debug profile
bash scripts/test.sh release all           # complete historical coverage
```

An optional first argument selects `debug` or `release`; the next argument is a
suite, or the remaining arguments are individual check IDs. Suite names and
check IDs cannot be mixed. Omitted selection means `quick`, including when only
`release` is supplied. This intentionally changes the old full-suite default;
old receipts retain their original meaning. Explicit checks run once in the
historical order. Every name is validated before tests or producer inspection;
help/list require neither HOME nor a build. The runner prints labels and whole
elapsed seconds without introducing a test scheduler.

| Suite | Coverage |
| --- | --- |
| `quick` | `protocol`, `provider-source`, `native-header` (C and C++), `typescript`: source/codec/compiler-JS checks without a product build or cold Rust compilation. |
| `core` | Quick plus producer fault cases, all matching-profile Rust workspace tests, managed path checks and the public TS/C#/CLI/MCP/native/coordinator/query baseline. Exact 27 IDs: `--list`. |
| `extended` | The remaining 20 matrix/adapter checks, including full npm/pnpm endpoints, direct C# context/SDK/target cases, deeper resolution/project-links/dispatch and experimental providers. |
| `all` | All 47 IDs / 48 commands, preserving their original order and cases. |

`rust` can compile its own tests without a prior product build. Other source-only
IDs also skip producer verification: `build-record`, `jsonrpc`,
`rust-navigation`, `typescript-classification`, `node-preparation`,
`preparation-safety`, `swift-toolchain`, and `rust-provider`. The last is an
experimental compiler-worker test, not a promise of a fast check. Pure does not
mean dependencies/toolchains are installed automatically or that every case is
cheap. All remaining IDs consume product build outputs. Before their first use,
and after the selected run, the existing build-record verifier checks matching
profile, inputs, generated files, tools and outputs. It never repairs/rebuilds a
producer silently. Formatting and strict Clippy remain explicit separate checks.

## Manual change triggers

Record the selected extra IDs and why. These are review rules, not automatic
Git-diff guesses. Shared code changes trigger its consumers too.

| Changed area | Add existing checks beyond `core` |
| --- | --- |
| Package selection/cache, npm/pnpm pins, restore safety or dependency policy | `node-preparation preparation-safety` with the complete supported ten-pin catalog; `provider-failures resolution` for affected setup/compiler boundaries. |
| TS compiler selection, inference/classification or provider failures | `typescript-classification provider-failures`; package checks above if setup/selection changed. |
| C# SDK/MSBuild evaluation, contexts, targets, classification, generated paths or resolution | `sdk-metadata csharp-context csharp-target-grouping csharp-classification resolution provider-failures`. |
| Fact identity, grouping, graph/body links, dispatch, coordinates or shared query evidence | `project-links dispatch provider-failures`; affected TS/C# classification/context checks above. |
| Shared LSP/BSP JSON-RPC, Node provider source/framing/process/environment contracts | `jsonrpc rust-navigation rust-provider swift-toolchain swift-provider swiftpm-provider xcode-provider languages`, plus affected TS/C# cases. |
| Experimental Rust/Swift/Xcode adapters or their toolchain selection | The relevant preceding adapter/toolchain cases and `languages`; no new language acceptance is implied. |

The npm/pnpm ten-cell matrix is independent of Rust Debug/Release compilation.
Run it once for unchanged relevant source/toolchain identities rather than twice
solely for the native profile. A `KNOW_CODE_TEST_PACKAGE_PINS` subset is focused
evidence and must not be reported as the complete matrix. Compiled C# or native
checks still require their matching profile. Current Snow Leopard changes touch
nearly all extra families: this split does not waive their current validation.

Core always retains actual trust, compiler/native source parity, calls/impact,
query admission/deadline/cancellation/response bounds, cursor replay, disk/resource
control, corruption recovery, upgrades, saved-file coordination and discovery.
Do not move those contracts out merely to shorten a release run. The script is
only one part of release validation: exact-package signing/notarization/license,
installed/offline checks, actual previous/current native compatibility and the
approved real-HOME lifecycle/security workflow remain separate existing gates.
No generic installer test that repurposes HOME is added. Encoding performance
remains explicitly deferred; this policy does not manufacture a passing result.

Observed Debug work was about 1,013 seconds to the failed resources fixture and
409 seconds for a later suffix after focused repair. Those are segmented costs,
not an uninterrupted green suite. Ten npm/pnpm cells dominate profile-independent
matrix work. The last pre-split full optimized run passed every command and
final producer verification at `c646bcf` in 1,135.4 seconds (about 18m55s), retained
in `.artifacts/snow-leopard/wave-01/full-release-01/summary.json`. That is the
historical full selection, not a measured duration for the new quick/core groups.
Preserve original failures and source/profile/command receipts for every selected run.

The tier selector was subsequently checked at `3b7960a`: all47 historical
commands are retained, parser errors precede work, and a named artifact check
verifies its producer before/after. The default quick selection passed in about
one second on this machine (`quick-tier-01`); core's elapsed duration has not been
measured separately. This is narrower coverage than `all`, not an equal-coverage
performance comparison.


The 0.0.3 `rust-managed` check is change-triggered for Cargo preparation, trust,
compiler-wrapper preservation and setup input/cache behavior. It uses the same
worker harness with owned local Git/path dependencies, tests scripts/macros only
under the managed policy, and retains malformed/oversized-cache and explicit
retry cases. It needs installed Rust/analyzer tools but no product native build
or hosted dependency download. Run it during relevant adapter work; it does not
replace assembled coordinator/native, exact Cargo-context or release gates.

The `rust-contexts` extended check runs owned compiler-context planning and
collection/cache fixtures after changes to Rust capture, cfg/input/artifact
proof or analyzer-project construction. It uses installed Cargo/rustc and
durable local fixtures, without a native product build or hosted downloads.
It covers warm reuse, include edits, replaced artifacts and failed-unit
isolation; selected-root worker and native graph parity remain separate gates.
It is intentionally excluded from routine quick and core checks.
