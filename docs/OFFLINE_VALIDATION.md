# Installed offline operation gate

`tests/package_install.py` extends the existing archive/consumer test rather than adding a second product harness. Its D02 boundary is dependency restoration followed by actual initial and saved-file analysis and public queries with networking and checkout access denied. This is not a signing, telemetry, performance or full release certification.

## Phase boundary

The harness installs the actual runtime, npm and NuGet archives into a durable prefix outside the tested checkout. It verifies archive hashes/native contents and retains the candidate manifest. When archives were built in another checkout, pass `--deny-checkout /absolute/build-source` so both the harness and actual build source are denied; each denied checkout must exist and its LICENSE read is checked. Setup installs the consumer SDKs, packs the runtime's pinned TypeScript compiler and a small local declaration dependency, restores the TypeScript project through npm and restores a real NuGet project dependency from the copied candidate feed. These are explicit setup operations; `--offline` flags on package tools are not the denial proof.

Before any Workspace registration or coordinator startup, the harness asserts that its new central data directory does not exist. Its phase record lists restored versions/assets and an empty serving-index set. It then changes the launch policy for every CLI, MCP and native consumer process. Their auto-started coordinator and compiler children inherit that policy. The first serving graph is therefore produced inside the denial boundary, not carried over from setup.

The required fixtures use configured TypeScript with a restored compiler/declaration dependency and C# with a restored NuGet dependency and compiler-integrated generated source. Both native SDKs and CLI must agree on fresh symbols, references and source context. Source checks compare exact UTF-8 ranges and full-content hashes against saved fixture bytes, including Unicode. Saved TypeScript and C# changes must replace the old semantic names, return current matching source, and be visible through MCP as well. A separate missing-restoration fixture must expose the missing package in a non-complete result/error while the healthy workspace remains queryable.

Existing SDK installation, native loading, runtime relocation, resource/manifest mismatch and missing-resource checks remain. Optional `--experimental` cases remain optional and do not expand 0.0.1 language acceptance.

## Denial and local IPC

The runtime profile denies `network*`, then permits only local Unix sockets beneath the isolated prefix and the SDK's GUID-shaped Roslyn BuildHost socket paths. Controlled loopback listeners prove working positive controls and explicit `EPERM`/`EACCES` denials for IPv4 and IPv6, TCP and UDP. The denied child also binds, connects and accepts both a prefix-scoped Unix socket and a GUID-shaped temporary Roslyn socket, and proves an unrelated temporary socket remains denied. The listeners must observe no packet/connection from the denied probes. No public Internet host is contacted by this proof.

The prefix exception covers coordinator sockets and `TMPDIR`-scoped helper IPC. Roslyn 5.9.0's pinned [NamedPipeUtil](https://raw.githubusercontent.com/dotnet/roslyn/35d9211b841e7613c1d2f8f5af6d628ace696c4c/src/Compilers/Shared/NamedPipeUtil.cs) chooses `/tmp` on Unix, and its [BuildHostProcessManager](https://raw.githubusercontent.com/dotnet/roslyn/35d9211b841e7613c1d2f8f5af6d628ace696c4c/src/Workspaces/MSBuild/Core/MSBuild/BuildHostProcessManager.cs) generates the pipe name as a GUID. The package's nuspec identifies that source revision. Consequently setting `TMPDIR` alone cannot relocate this SDK IPC. The profile permits only that temporary Unix socket pattern under `/tmp` and `/private/tmp`; retained work and evidence stay in durable locations. This grants no IP networking, general `/tmp` socket access or resolver socket access. It is an offline-operation test for trusted tools, not a security claim against an adversarial local proxy/broker.

`network-policy-proof.json` records both controls, Unix success and the profile hash. The profile itself is retained. Package tools, SDKs, coordinator and workers use the same network-denied runtime phase; no already-running unsandboxed coordinator can supply the initial graph. The unique data root and returned database paths are checked.

## Run and evidence

On macOS ARM64 with the declared Node/.NET prerequisites, use a fresh, short durable prefix outside the tested checkout:

```sh
python3 tests/package_install.py /absolute/candidate-packages \
  --prefix /Users/you/Code/.know-code-package-tests/offline-run
```

The outer agent sandbox may prohibit `sandbox_apply`, Unix sockets or FSEvents. Use supported escalation for the harness; the child test policy remains explicitly restrictive. Do not weaken Internet denial or count a package-tool offline flag as proof. Reusable C# compiler/MSBuild servers stay disabled.

The prefix retains `candidate-manifest.json`, `commands.jsonl` with setup/runtime phase labels, `phases.json`, sandbox profiles/probes, SDK/MCP evidence, missing-restoration diagnostics and a final `result.json`. The worktree's `.artifacts/packaging/installed-test.json` points to the latest completed run. Failed attempts retain their separate prefix/logs and are not reported as passes.

Development may use frozen archives under `.artifacts/cancellation-boundary/1794273886245465000/packages/`; their manifest reports source revision `c484e6f5377e82d6fbe723cdf3b5a17758b74d57`. A passing run on those archives proves the harness and that historical candidate only. The assembled release must rerun this gate against its own provenance-verified archived bytes, final compiler/package matrix and selected platform.

## Current evidence

The complete corrected historical-archive run passed **22 checks** at harness revision `5cd285c`, with frozen archive manifest source revision `c484e6f5377e82d6fbe723cdf3b5a17758b74d57`. Promoted evidence in the primary repository is [.artifacts/release-offline/1794330718012677000/validation.json](../.artifacts/release-offline/1794330718012677000/validation.json); the durable installed prefix is `/Users/zakkeown/Code/.kco/r4800b9e960`. Both this worktree and the actual primary build-source checkout were denied. The coordinator socket is absent after cleanup; the live development installation and original source corpora were untouched.

The run proves fresh initial TS/C# compilation, generated source, exact Unicode source hashes/ranges, references, native/CLI/MCP agreement, automatic saved changes in both languages, relocation/reconnection and actionable missing restoration while the healthy workspace stays queryable. The missing-dependency case published an explicitly non-complete graph with the missing package named in coverage; a successful CLI transport was not interpreted as complete analysis. All four IPv4/IPv6 TCP/UDP probes returned policy denial, both required Unix controls succeeded, and an unrelated temporary Unix socket remained denied.

Before the final run, corrected helpers were replayed against 11 captured CLI and 22 native query envelopes, with exact initial fixture bytes for source/hash/range checks. Rejection controls cover `not_ready`, `deadline_exceeded`, missing-result, stale and older-generation envelopes. The final harness routes MCP structured results through the same success/currentness/generation check and retains actual wire evidence in `mcp.json`. This replay is retained under the final run's `assertion-replay/` directory. Independent review then tightened initial coverage to require a nonempty complete set and source slices to require explicit ordered bounds plus nonempty body text. Those assertion-only refinements were replayed against the completed run: 18 CLI queries, 35 native query envelopes, three actual MCP query envelopes and four exact source slices pass; controls reject empty/partial coverage and invalid/empty ranges. This is captured-result replay, not a new runtime run on the refined harness.

Four earlier attempts remain failed and retained:

| Worktree artifact run | Outcome |
| --- | --- |
| `1794328973540838000` | Prefix-only Unix policy denied Roslyn's fixed temporary pipe; no publication. |
| `1794329496994021000` | Seatbelt accepted `{n}` syntax but did not match the GUID path; no publication. Exact literal/simple/explicit-repetition probes distinguish that policy issue. |
| `1794329970283377000` | Fresh offline graph existed; a fixture wrongly treated workspace-relative source paths as absolute. Corrected resolution preserves exact hash/range checks. |
| `1794330111527266000` | Initial and saved TypeScript graphs existed; a fixture indexed an omitted empty `symbols` array directly. Corrected absence checks first require a successful current result at the expected generation floor, then require exactly empty/omitted symbols. |

No current assembled candidate, optimized build, full Debug suite, older-OS guest, performance, signing/notarization or public-build telemetry certification is claimed. Re-run the gate on the final provenance-verified candidate after integration. The `matrix` field retained from the archive manifest describes producer metadata; actual installed/relocated runtime discovery is retained in command output and the validation record.
