# Native runtime bindings

Status: accepted C ABI/PInvoke and Node-API mechanisms are implemented for the [first slice](IMPLEMENTATION.md). Both execute the same Rust queries in-process. The [Workspace API](WORKSPACE_API.md) now adds shared registration, startup, progress, refresh/wait, typed errors and client disposal. Cursor lifecycle, cancellation, final DTO/version design and distribution packaging remain unfinished.

## Accepted integration shape

```text
.NET API -> C ABI / PInvoke bridge -----+
                                      +-> same Rust core -> local SQLite read view
Node API -> native Node-API add-on -----+
                                      +-> shared Rust control client
                                           -> coordinator Unix socket
```

Each application embeds the core in its own process. The coordinator supervises updates/workers independently. Bindings do not implement SQLite queries, graph algorithms, invalidation, or independent coordinator clients.

The diagram is the accepted target shape. The saved-state slice implements a persistent coordinator and shared Rust control client. Native queries remain in-process, with bounded freshness waits and stale fallback; see [coordinator details](COORDINATOR.md).

Accepted initial packaging: one Rust core crate with ordinary internal modules, plus thin host/native-binding targets. CLI/MCP-only dependencies stay outside the embedded core's required dependency surface. Native wrappers may need separate build targets/artifacts; their exact package layout is not selected, and each wrapper must reuse the same core rather than create its own domain engine.

## .NET mechanism and proposed details

- Build a native system library from Rust with a small explicitly versioned C-compatible ABI.
- Use P/Invoke, with `LibraryImport` a candidate mechanism, behind ordinary typed managed APIs.
- Represent long-lived engine/workspace/query-cursor state as opaque native handles with explicit managed disposal and native release functions. `SafeHandle` is a candidate lifetime wrapper.
- Pass coarse operations/batches rather than making one interop call per node, edge, or field.
- Define error/buffer ownership, ABI version compatibility, and failure behavior; Rust-specific layouts or unwinding must not cross the unmanaged boundary.

ABI v1 exports `kc_abi_version`, `kc_query_json`, additive `kc_workspace_json`, and `kc_string_free`: coarse UTF-8 JSON requests/results with explicit native string ownership. The .NET 10 wrapper checks ABI version, uses LibraryImport, offloads queries with Task.Run, and frees each native result in finally. Workspace clients also use coarse native calls and own no long-lived native handles; disposal closes client admission while accepted calls finish. Final DTO encoding, cancellation, broader runtime support, and generated declarations remain open. A C ABI is an interoperability boundary, not a requirement to implement core logic in C.

## Node.js mechanism and proposed details

- Provide a native add-on using Node-API and typed JS/TS-facing wrappers.
- Invoke the same Rust core directly; do not implement a JS graph/store or require ordinary graph queries to be sent through the coordinator.
- Keep Node-specific types/conversion in the binding layer. The Rust model/storage code must not depend on JS/V8 object semantics.
- Return bounded result pages/buffers. Ensure file I/O, freshness waits, and graph work do not block the JS event loop.
- Tie resource cleanup to explicit close/dispose behavior and environment shutdown; GC alone should not indefinitely pin old SQLite views.

The first slice uses napi-rs 3 (locked versions), Node-API 8, libuv async work via AsyncTask, and JS/TS wrappers, tested on Node 24. Coarse JSON requests/results reuse the Rust API; read views close within each task. Broader Node support, cancellation, generated type declarations, and final DTO encoding remain open. Node-API's stability does not automatically version Know Code's own ABI/schema or every dependency.

## Shared boundary rules

- One semantic request/result model, mapped into host-native forms. Derive declarations/schemas where practical without building a general code-generation framework.
- Keep validation at real boundaries. Host decoding and core operation invariants have different responsibilities; avoid repeating all checks in every wrapper.
- Common error categories preserve actionable prerequisite/setup errors, invalid scopes, not-ready state, cancellation/deadline, cursor expiry, and incompatible versions.
- Accepted v0.0.x compatibility policy: compatible library/coordinator versions may coexist; incompatible pairs return an actionable update-required error. Broad backward compatibility is not required before APIs settle. Shared Rust compatibility handling preserves component/version/boundary information across both wrappers; exact ranges and handshake design remain open.
- Freshness wait and generation-pinned pagination behave identically in both bindings. A wrapper must not silently restart expired pages on a new generation.
- Explicit cancellation/disposal cannot invalidate another client's active work or cancel shared background indexing.
- Resource ownership must remain safe when a client closes while native work is active. Define how completion/cancellation drains handles and owned buffers.
- Avoid unnecessary serialization/copying for in-process calls. The accepted Protobuf worker/control protocol does not require serializing every Rust function call.
- Public distribution artifacts have zero telemetry. Debug/personal instrumentation uses the accepted local collection/rotation policy across bindings and core.

These are design requirements/directions, not claims that cleanup, async, or ABI guarantees are already implemented.

## Packaging and performance work

v0.0.1 targets macOS/Apple Silicon. Native add-on/system-library loading, package metadata, coordinator/core schema compatibility, build/profile identity, and upgrade behavior need explicit design. Packaging might use distinct native artifacts linked from the same core source; sharing an implementation does not require the same physical library file in every package.

Accepted SQLite packaging: coordinator and native library artifacts bundle a pinned SQLite build with FTS5, using the same release pin/build-feature policy. Users do not need to install a separate SQLite package, and native loading must resolve the intended bundled implementation. This does not require a single physical shared library across all packages. Exact version, Rust binding crate, static/shared linkage, and artifact-size tradeoffs remain implementation work.

Benchmark matched core/.NET/Node queries for result/evidence equivalence, total allocations/copies, repeated-query overhead, cancellation, disposal races, cursor expiry, and aggregate memory. Include active publication and coordinator restart cases. Do not infer performance from an interop mechanism alone.

## Primary references

- [Rust linkage](https://doc.rust-lang.org/reference/linkage.html): system-library artifacts for loading/linking from other languages.
- [.NET P/Invoke](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke): native function calls and generated import declarations.
- [Node-API](https://nodejs.org/api/n-api.html): native add-ons and scope/limitations of ABI stability.

See [implementation/build/test instructions](IMPLEMENTATION.md) for the working development artifacts and their current limits. No packages have been published. The user-approved development service is installed as documented in [LOCAL_INSTALL.md](LOCAL_INSTALL.md).

## Workspace control and startup

The user accepted automatic startup of an installed coordinator and typed exceptions for the new Workspace API; the existing low-level Index API keeps JSON envelopes. Both SDKs call the same native `workspace_json` operation; CLI/MCP attachment now reuses its Rust registration/bootstrap helper. See [examples, error/ownership contract, installation overrides, reconnect behavior and explicit remaining limits](WORKSPACE_API.md). No worker, query engine, watcher or state database was added to either managed wrapper.
