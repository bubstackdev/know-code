# Releasing KnowCode

This procedure prepares a release; it does not authorize the first public push.
The audited snapshot must pass the private-vocabulary scan and manual review by
both the assistant and the user before publication. Private development history
must never be pushed to the new public repository.

## Build and distribution

[dist-workspace.toml](../dist-workspace.toml) pins cargo-dist and selects
`knowcode` 0.1.0 for Apple-silicon macOS and Linux x86_64/arm64. It generates
gzip archives, SHA-256 checksums, and a shell installer. Homebrew distribution
is deferred to 0.1.1.
Both [MIT](../LICENSE-MIT) and [Apache-2.0](../LICENSE-APACHE) licenses accompany
the archives. There is no separate updater binary or background daemon.

Local packaging and CI invoke `scripts/build-release` to remap builder home,
Cargo cache, and checkout prefixes in source-location strings: rustc's path-prefix
remapping for Rust, and `-ffile-prefix-map` in `CFLAGS` and `CXXFLAGS` for the C code the
`cc` crate compiles (tree-sitter and its grammars), appended to any flags the
caller set. The wrapper turns on `CC_SHELL_ESCAPED_FLAGS` so a prefix containing
spaces can be quoted. Debug-symbol stripping alone does not remove these paths.
Each native release build then runs `scripts/check-build-paths.py`, which fails
before attestation if any file in its archives contains the builder's home,
Cargo home, or checkout path. Every final artifact still needs the
private-vocabulary scan.

Install the pinned version of [cargo-dist](https://axodotdev.github.io/cargo-dist/book/install.html)
for release development. Regenerate the workflow with:

```sh
python3 scripts/generate-release.py
python3 scripts/generate-release.py --check
dist plan --output-format=json --no-local-paths
```

Use this wrapper instead of running `dist generate` directly. The generated
release workflow calls the existing CI gates before builds and additionally
requires their success and successful local/global builds at the publishing step.
That explicit condition prevents failed gates from being mistaken for intentionally skipped builds. The config's
`allow-dirty = ["ci"]` exists solely to preserve this reviewed customization.

The wrapper also hardens the generated workflow: contents are read-only except in the job that creates the
GitHub Release (the build job's attestation permissions are cargo-dist's own); cargo-dist is installed by
`scripts/install-dist.sh`, which runs its installer only when it matches the SHA-256 pinned there for the version
in `dist-workspace.toml` (a new version needs its digest added); and the tag reaches shell steps through `env:`,
never spliced into the script. The workflow publishes nothing outside the GitHub Release and uses no secret
besides its own `GITHUB_TOKEN`. `scripts/test_dist_build.py` checks all of this against the checked-in workflow.

The workflow uses immutable commits of GitHub-owned Actions. Local archives
build on native runners; installer generation follows all platform builds so it
can embed their checksums. A local global-only build made before those archives
exist is a preview and must not be published: it lacks the other targets' hashes.

For a native local packaging check:

```sh
bash scripts/package-native.sh
python3 scripts/check-installer.py target/distrib
python3 scripts/check-updates.py target/debug/knowcode
```

The helper selects the host's supported target and preserves its local manifest
before generating the installer, as release CI does. That handoff is required to
embed checksums. Do not publish a local partial build. CI builds the entire target matrix before uploading a
release. GitHub artifact attestations are enabled for its platform archives.

The update check test intercepts curl locally; it makes no network requests and only changes test-owned
installations. It covers daily coordination, opt-out, shell receipt ownership, checksum rejection, failed-install
rollback, and cancellation of an installer's own descendants. A real cross-version update remains a release
acceptance check once two reviewed public releases exist.

## Installation

Version 0.1.0 ships the shell installer only; Homebrew returns in 0.1.1. The planned
installation command is:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bubstackdev/knowcode/releases/latest/download/knowcode-installer.sh | sh
```

After installation, run `knowcode setup claude`, `knowcode setup codex`, or
`knowcode setup cursor`. The client session's first query indexes automatically.

## Changelog

Add the release's entry to [CHANGELOG.md](../CHANGELOG.md) before tagging.

## Publication and evaluation gates

Keep public content descriptive, without comparative claims or evaluation
numbers, until the supporting public evaluation exists. Publishing version 0.1.0
does not close the release goal: the reproducible public evaluation, including
failures and losses, must also be published.

Before creating the release tag, verify the clean-machine first-run checks,
following [first-run-acceptance.md](first-run-acceptance.md),
the audited snapshot and user review, all local gates ([evaluation.md](evaluation.md#gates-per-change)), and
the public CI run. Public Actions execution remains unverified until the approved repository exists. Tagging `v0.1.0` triggers publication through
the workflow, so do not push that tag until those gates are satisfied.
