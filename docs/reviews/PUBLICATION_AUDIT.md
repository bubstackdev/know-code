# Pre-tag publication audit

Read-only source review at `ccea54a`, followed by the packaging-only notice fix
`89e00d5`. No build, package, signing action or native test was run by this
reviewer. The pending npm 10 scripts-off decision is tracked separately in
[NPM_SCRIPTS_OFF.md](NPM_SCRIPTS_OFF.md).

One concrete publication blocker was found: three bundled NuGet dependencies
had only nuspec SPDX/URL metadata in their notice inventory. `89e00d5` adds
unchanged upstream license texts anchored to each restored package's exact
repository commit, and makes packaging reject missing/mismatched text evidence.
No other concrete blocker was established within this review's scope.

## Source findings

- No Know Code usage-event collector, hidden local usage log, telemetry upload
  sink, or runtime opt-in collector was found in the core, hosts, bindings or
  provider sources. [DIAGNOSTICS.md](../DIAGNOSTICS.md) describes future personal
  diagnostics; it is not an implemented collector or a public feature toggle.
  Current registry/settings, compiler reports, freshness state, resource
  enforcement, query admission and expiring cursor files serve required product
  behavior. They are not a persistent usage-history facility.
- The Rust core's dependency surface has no telemetry SDK. Source searches and
  the pinned npm/NuGet/Cargo manifests found no configured analytics backend.
  Worker launchers explicitly disable .NET CLI telemetry; package-tool calls
  disable update notifiers. Registry/tool acquisition and requested dependency
  restoration remain network operations, distinct from telemetry. This is not
  a promise about arbitrary personally trusted project code or user-installed
  compilers outside Know Code's control.
- Node `test-hooks` is an explicit non-default Cargo feature. The module, task
  fields and hook calls are guarded by that feature. `build_record.py`'s normal
  Cargo invocation does not enable it. The SDK npm file allowlist excludes the
  diagnostic source/driver. The runtime copies only provider `.mjs` modules,
  protocol/native helpers and the declared pinned runtime dependency closure;
  it does not copy reference clones, owned fixtures or build directories.
- Package assembly checks matching producer inputs/outputs before and after
  copying; npm and NuGet native payload bytes are matched against the producer.
  Notices are copied into the runtime, npm and NuGet artifacts. Packaging
  manifests and provenance declarations support these checks; a field such as
  `telemetry: none` is not itself proof of the binary's behavior.

## Notice evidence

The historical graph-source candidate inventory
`443ec43d969cb1a92bc1b5fe27ead40dab3a3cbb86d3fadf6058953b6a397f36`
contains 137 entries: 17 npm, 23 NuGet and 97 Cargo. Its nuspec-only omissions
were Google.Protobuf 3.36.2, Humanizer.Core 2.14.1 and Microsoft.Build.Locator
1.11.2. Current restored assets retain those versions. Their exact upstream
commit, URL, license identity and content SHA are now in
[the existing pinned index](../../third_party/licenses/index.json).

`package.py::pinned_notice` verifies the restored commit/license identity,
bounded repository-owned notice path and actual text SHA. NuGet retains its
nuspec alongside the real text and records `notice_provenance`; Cargo reuses
the same helper. Existing package-input inventory automatically covers the
new files. This packaging-only change does not by itself require rebuilding
the binaries. The 137-entry historical inventory is evidence of the discovered
gap, not certification of an unbuilt final release inventory.

## Exact final-artifact checks still required

1. Package from the final matching **release** producer into a fresh directory.
   Inspect runtime tar, npm tarball and NuGet zip directly: root Apache-2.0
   LICENSE, complete THIRD_PARTY inventory/text paths, and the three exact new
   upstream texts/provenance must exist. Reject missing entries, nuspec-only
   evidence, escaping paths and copied diagnostic fixtures. Bind archive and
   producer hashes before/after any signing transformation.
2. Load the actual packaged Node add-on in an isolated owned scope and inspect
   its own properties plus the known diagnostic export names. It must expose
   contract 8 and no test exports. The root's seven-group diagnostic/default
   gate passed on the named Debug add-ons; that does not certify different
   optimized or signed archive bytes. Keep the diagnostic add-on separate from
   producer outputs and distribution staging.
3. Run the final installed/offline acceptance on those exact archives after
   dependency setup, with checkout and IPv4/IPv6 TCP/UDP access denied and only
   required local Unix IPC permitted. Retain actual denial controls and query/
   save results. Source inspection plus that finite observation supports the
   reviewed zero-collector/local-operation claim; it is not exhaustive dynamic
   proof over every future user project or prerequisite tool.

No final archive, optimized/platform/performance result, authenticated signing,
notarization or publication approval is certified by this source audit.
