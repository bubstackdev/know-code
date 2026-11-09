# Local diagnostics and zero-telemetry public builds

Status: public/debug policy, unrestricted local payloads, and automatic budget-based rotation accepted. Initial design budget: configurable 10 GiB per user. Event schema, capture/rotation mechanics, and build flags remain design work.

## Confirmed policy

- Know Code operates locally once project dependencies are restored; it requires no hosted analysis/query service.
- The widely published version must have zero telemetry. This includes silent local usage collection as well as remote collection; default-off collection is not the same as zero collection.
- Debug builds may gather extensive telemetry locally.
- The user's personal install may also gather local usage/diagnostic telemetry, including an optimized personal build if useful.
- The purpose is to let future agents inspect actual usage and find/reproduce Know Code bugs.
- The user explicitly permits these local diagnostics to capture "literally anything": full query text, request/response payloads, source contents, compiler/worker inputs and outputs, configuration/environment context, and internal state relevant to Know Code behavior. No metadata-only restriction applies.
- No automatic uploading or hosted telemetry backend is authorized or required.
- Rotate automatically under a reasonably generous disk budget. The initial design default is 10 GiB, configurable, covering all local diagnostic records, payload files, and reproduction bundles across workspaces for that user. This is separate from live index/registry storage.

Ordinary user-facing errors, requested status/progress, compiler diagnostics necessary to explain index coverage, and required index/registry state remain product capabilities. Do not use that distinction to hide a persistent usage tracker under another name.

## One engine, optional instrumentation

Use the same Rust engine, analysis contract, and query behavior in all profiles. Do not create alternate diagnostic indexing/query pipelines or a general observability framework.

Proposed enforcement: compile usage-collection callsites/sinks out of public artifacts, including the coordinator, bindings, and language workers. Public distributions must not gain telemetry through an environment variable or hidden runtime switch. Personal/debug collection should be explicit in build/profile identity. Exact feature flags, worker propagation, and packaging checks are undecided.

## Proposed investigation records

Versioned structured records should be straightforward for agents to read and correlate, rather than requiring a hosted dashboard. A bounded JSONL event stream plus a small run manifest is a candidate format, not an accepted storage dependency.

Useful candidate fields/categories:

- Know Code build/commit/profile, platform, provider/compiler/runtime versions, and relevant configuration fingerprints.
- Request/job/correlation IDs joining bindings, core, coordinator, restoration, and analysis workers.
- Workspace/compilation identity, observed input and served generation, source/configuration fingerprints.
- Save/event detection, invalidation decisions, queue transitions, analysis/publication timing, and freshness waits/fallbacks.
- Query operation, limits, query-plan/candidate/visited/result counts, truncation, and coverage.
- Errors, crashes/restarts, cancellation/timeouts, restoration failures, and publication/recovery outcomes.
- Memory/CPU/I/O observations sufficient to investigate resource growth and repeated work.

Full query text, source snippets/files, raw request/result payloads, relevant environment/configuration, dependency-manager output, and internal analysis state are permitted in local Debug/personal records. Choose useful capture points for investigation/reproduction without imposing a metadata-only filter or building a generalized content-validation subsystem. Unrestricted payload permission does not itself select capture frequency, serialization format, or indefinite retention.

## Reproduction limits and retained state

A trace pointing only at a current database cannot guarantee later reproduction after source or index changes. Full input/source/fact snapshots and request/response bundles are allowed for reproduction. Record versions/fingerprints and what was actually retained so agents can distinguish exact replay from incomplete evidence. Bundle layout and automatic capture triggers remain open.

Diagnostic retention is distinct from searchable historical code revisions. Diagnostic snapshots may preserve source/inputs needed for bugs without implementing the product's future historical-query feature. Store retained records in durable central application data; do not reference temporary/reboot-cleaned paths. Automatic disk-budget rotation is selected; it does not imply indefinite retention.

## Rotation behavior

- Initial configurable budget: 10 GiB per user across registered workspaces, including diagnostic source/payload snapshots and reproduction bundles, rather than 10 GiB per repository or worker.
- Rotate oldest completed segments/runs/bundles first. Group related evidence where practical and mark gaps if a retained trace is incomplete. No specific time-based retention guarantee is implied by a size budget.
- Account for active writes and oversized payloads; a single unbounded event must not defeat the budget. Exact segment sizing, reservation, and oversized-payload handling remain implementation details. Record size/drop/pruning information when evidence cannot be retained.
- Keep a bounded summary/manifest of retained evidence and pruning so agents can identify missing time ranges, payloads, or correlations. Do not report an incomplete trace as a reproducible run.
- Rotation applies only to the owned diagnostics subtree. It must not delete source repositories, live index databases, coordinator registration, or generations pinned by active queries.
- Collection overhead and disk pressure must not cause unlimited queues or block the engine indefinitely. Measure overhead separately from zero-telemetry public builds.

This is one local retention policy, not a generalized archive or observability framework. Manual export/pinning behavior, if needed later, requires explicit budget accounting rather than bypassing rotation silently.

## Agent workflow to enable

1. Identify a run/build and inspect its manifest.
2. Follow one request or indexing job across processes using shared IDs.
3. Compare observed inputs, invalidation, worker output, published generation, and query evidence.
4. Diagnose stale results, wrong relationships, crashes, repeated restore/index loops, or resource growth.
5. Reproduce with available inputs or explicitly identify what is missing.
6. Convert a confirmed defect into a focused regression case in the common engine/provider code.

Reading these records is user-directed local investigation. This design adds no autonomous remote reporting or separate agent orchestration system.
