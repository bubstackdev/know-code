# Know Code

Know Code gives agents and applications a local, compiler-backed code index. A shared Rust core provides symbol and signature search, references, callers, callees and dependency-impact queries through a CLI, a stdio MCP server, and in-process Node.js and .NET libraries.

Compiler adapters preserve source locations, compilation/target provenance, diagnostics and coverage. Call/dependency/dispatch coverage is explicitly partial; see the [supported graph forms and limits](docs/CALL_GRAPH.md).

**Published v0.0.2, Snow Leopard, supports TypeScript 5.9.x and C#.** Its Swift/Rust adapters remain experimental. See the [changelog](CHANGELOG.md) and [review ledger](docs/CODEBASE_REVIEW_0_0_2.md).

**This checkout is the unreleased v0.0.3 candidate.** It adds TypeScript 6.0.x and exact native 7.0.2, plus useful Swift/SwiftPM/Xcode and Cargo Rust graphs with explicit partial boundaries. Package versions and required-provider declarations do not establish a published release. Final artifact, installed/offline, upgrade/platform and corpus qualification remain pending; failed and interrupted attempts are retained in the [candidate ledger](docs/IMPLEMENTATION_0_0_3.md) and [release audit](docs/RELEASE_0_0_3_AUDIT.md).

## Get started

Know Code targets **macOS 15+ on Apple Silicon (ARM64)**. Install **Node.js 24** and the **.NET 10 SDK** separately and put them on PATH. Know Code detects missing or incompatible prerequisites; it does not install them. Follow the [finite compiler contract](docs/COMPILER_ACCEPTANCE.md) for your release and repository-selected toolchain.

Candidate Swift indexing additionally needs the selected Swift compiler/SDK; Cargo indexing needs repository-selected Cargo/rustc, rust-analyzer and matching rust-src. Xcode indexing requires the separately installed pinned [Xcode bridge](tooling/xcode-bridge/README.md), planned as an optional separate download and DMG. SwiftPM and ordinary queries do not require that tool. Current Xcode evidence is specific to the tested toolchain; the common runtime's macOS floor does not certify every Xcode combination.

For packaged installation, use the notarized, stapled DMG from [GitHub Releases](https://github.com/bubstackdev/know-code/releases) and follow that release's verification instructions and the [installation guide](docs/INSTALLATION.md). Runtime and native SDK archives are also available there. The runtime is installed once per user; the npm and NuGet SDK archives are separate consumers of that runtime.

From the extracted runtime:

```sh
/path/to/extracted-runtime/bin/know-code install
~/.local/bin/know-code install-info
~/.local/bin/know-code trust /absolute/repository
~/.local/bin/know-code watch /absolute/repository
```

Grant project trust only after reviewing the repository: compiler loading, MSBuild evaluation and generators can execute repository-selected code. Opening or registering a workspace does not grant this permission. Package scripts and hooks require a **separate personal permission**, `allow_install_scripts`, which defaults to disabled; checked-in settings cannot grant it. See [workspace settings and trust](docs/WORKSPACE_SETTINGS.md).

Installation enables login startup by default; `install --no-login` selects on-demand operation. The installer also supports updates, compatible rollback and uninstall. Uninstall retains indexes, workspace registrations and settings. See the [lifecycle commands and limits](docs/INSTALLATION.md).

## Connect an agent or application

Configure your agent to launch the installed executable with an explicit workspace:

```sh
~/.local/bin/know-code mcp --workspace /absolute/repository
```

The MCP process uses stdin/stdout and waits for its client. [MCP setup and tools](docs/MCP.md) cover symbol/reference lookup, callers/callees/impact, bounded structured queries and source excerpts. HTTP transport remains outside the release scope.

Embed the same Rust operators with the native Node-API add-on (`@know-code/node`) or .NET library (`KnowCode`). [Workspace API examples](docs/WORKSPACE_API.md) show opening a repository, granting trust, waiting for indexing, querying and disposing clients. Storage and graph queries execute in the calling process; compiler analysis uses isolated workers.

## Saved code, visible evidence

The persistent coordinator watches saved files and keeps indexing after clients disconnect. Local processes share one index per repository/worktree, stored centrally per user outside source repositories. Dependency restoration has visible progress and failures; operation is local after dependencies are restored, with no hosted service required.

Queries use coherent index generations. By default they wait up to 500 ms for pending saved changes, then return usable prior results explicitly marked stale; without a usable index they return `not_ready`. Freshness and completeness are separate: current results can contain partial or unavailable compilation coverage. Inspect coverage, diagnostics, target provenance and query limits alongside findings. Unsaved editor buffers are outside scope, and a static dependency or possible dispatch target is not proof of runtime execution.

Locals, parameters, test code and generated project source are included, with origin/classification evidence and shared query filters. The [implementation record](docs/IMPLEMENTATION.md) describes actual provider and query boundaries. [Shared resource controls](docs/RESOURCE_CONTROL.md) use adjustable two-core/2 GiB background defaults across workspaces; interruptions preserve coherent prior results with explicit stale/resource status.

Public builds have **zero telemetry**, including local usage collection and hidden opt-in collectors. The separate Debug/personal diagnostic policy is described in [diagnostics](docs/DIAGNOSTICS.md).

## Build and roadmap

To build and verify this checkout:

```sh
bash scripts/build.sh
bash scripts/test.sh
```

Pass `release` for optimized artifacts. The test script defaults to quick checks; [validation tiers](docs/VALIDATION.md) define the required release baseline and extended matrices. The product build profile is separate from an indexed workspace's C# configuration. [Packaging](docs/PACKAGING.md) describes the runtime/SDK layout and artifact provenance.

[Swift/SwiftPM](docs/SWIFT_PROVIDER.md), [Xcode](docs/XCODE_PROVIDER.md) and [Rust](docs/RUST_PROVIDER.md) document the candidate's finite evidence and limits. Cross-repository search remains optional for 0.0.3; company onboarding, Angular template references and refactoring continuity remain backlog work. Embedding-based semantic search and searchable source history remain outside the current release scope.

See [design requirements](docs/DESIGN.md), [architecture](docs/ARCHITECTURE.md), [release plan](docs/RELEASE_PLAN.md) and [performance evaluation](docs/PERFORMANCE.md) for the durable requirements, roadmap and evidence boundaries.

## License

Apache-2.0. See [LICENSE](LICENSE). Local research clones retain their upstream licenses.
