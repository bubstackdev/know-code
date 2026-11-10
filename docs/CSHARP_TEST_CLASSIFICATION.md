# C# project test classification

This adapter implements policy1 from [TEST_CLASSIFICATION.md](TEST_CLASSIFICATION.md). It emits saved compiler-context evidence and does not run tests, change indexed code selection or implement query filters. Root owns storage, mixed-declaration aggregation, filtering and backfill.

The existing SDK helper reads `IsTestProject` and `IsTestingPlatformApplication` from the final per-target project state after the normal `GetAssemblyVersion` metadata hooks. Planning before restoration emits no classification. Non-.NET-SDK projects use their final evaluated target state when no identity target is run. No second evaluation or Roslyn workspace is introduced.

| IsTestProject | IsTestingPlatformApplication | Kind |
| --- | --- | --- |
| true | any | test |
| false | true | unknown (conflict) |
| false | false/unknown | non_test |
| unknown | true | test |
| unknown | false/unknown | unknown |

Missing, empty, invalid and oversized signals remain unknown. Evidence records actual available properties, raw values, target, evaluation stage and normalized project path; it makes no package/directory/framework-name guesses. Evidence is bounded in UTF-8, with explicit abbreviation of oversized values; abbreviation never turns an oversized signal into a valid boolean. A known kind reports `test_classification:complete`; observed but inconclusive signals report partial; no evidence or legacy policy reports unknown. Compiler diagnostic completeness is independent of this capability.

Classification travels separately from `SdkTargetMetadata.Identity`, which supplies compiler globals and batching comparisons. Per-ProjectId metadata keeps identical linked text independent across production/test targets. Ordinary, generated and linked documents inherit their owning compilation and carry no source override. Source bytes/hashes, artifact IDs, symbols and coordinates remain unchanged. Policy0 emits no compilation classification; unsupported policy versions fail before SDK work/facts. The requested policy is acknowledged before fact emission.

Effective policy and classification/evidence join every project snapshot, including recursively referenced contexts. Existing imported props/targets remain in `KnowCodeInputPaths`/compilation inputs. Saved flags, imports and configuration/target selection use the existing invalidation/publication path. Classification policy is separate from preparation policy and does not itself force a restore. This does not extend the existing guarantee to arbitrary environment/tool changes outside tracked inputs.

Focused assertions are in `tests/sdk-metadata.mjs` (truth table, UTF-8 bounds, final-hook state, identity separation) and `tests/csharp_classification.mjs` (all targets, exact acknowledgement, generated/linked inheritance, bytes/ranges, legacy/preparation reuse, own/referenced witnesses, partial graphs and unsupported versions). The latter holds its request fingerprint constant to isolate classification in snapshots; root still must verify real coordinator invalidation and all query surfaces. Results and exact source/worker identities belong in the packet handoff; syntax-only checks are not compiler acceptance.

Isolated Debug validation passed for implementation `5167a13` and final direct-worker fixture `edafa38`: provider-only managed build with zero warnings/errors, 10 SDK metadata groups and 8 direct-worker groups. The latter includes policy1 unmarked/empty signals with unknown/partial capability in the same five compilation contexts. Effective compiler was Roslyn5.9.0.0 / SDK10.0.403; worker SHA-256 `4311ed0f6718462708803b8f91115537638dcd39c6091a6f9fa8155d6e1c45e8`. [The durable handoff](../.artifacts/csharp-test-classification/1a1c73e7723/handoff.json) retains exact sources, generated protocol/output identities, commands/logs and full framed fixture messages. No native/product build, live coordinator, original corpus or broad suite was used. Combined root storage/filter/backfill/native acceptance remains required.
