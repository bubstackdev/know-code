# Local research corpus

Reference repositories live under `repos/`, inside the durable project workspace. They are ignored by Know Code's Git repository; research findings, this file, and `repositories.json` should be retained with the project.

`repositories.json` records each clone's origin, exact reviewed commit, and relative local path. Online source citations in `docs/SOURCE_REVIEW.md` are pinned to those commits so findings remain usable without a local clone.

To restore a missing reference, clone its recorded origin into its recorded path and check out the recorded commit. A shallow clone of a future default branch may require fetching the recorded commit before checkout. Do not silently substitute a newer revision when reproducing findings.

These repositories are source references, not Know Code dependencies or vendored implementation. No builds, installs, or reference-project tests were run as part of the initial crawl. Read upstream component licenses before any future code reuse.

Do not move this corpus or any retained research artifacts into temporary or reboot-cleaned directories.

## Public indexing corpora

[corpora.json](corpora.json) separately pins ts-pattern (TypeScript) and Polly (C#), cloned to exercise Know Code's compiler analysis rather than serve as implementation references. Both are registered with the development login coordinator, alongside Know Code itself. Dependency restoration was attempted for these two corpora; no upstream full builds or test suites were invoked. Their original licenses remain in their clones.

See [corpus findings](../docs/CORPUS_FINDINGS.md) for setup, actual coverage, and product gaps. Keep upstream source revisions unchanged when reproducing results; generated dependency/install outputs are local setup, not reviewed upstream commits.

Pinned Roslyn/MinVer source subsets used for the compiler-resolution investigation live under ignored `sources/`; [source-references.json](source-references.json) retains their revisions and licenses. They are reference material, not vendored implementation. See [Polly resolution findings](../docs/POLLY_RESOLUTION.md).
