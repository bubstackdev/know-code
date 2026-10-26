# Contributing

Thanks for helping. Bug reports with a public repository that reproduces them are the most useful thing you can
send: name the repository, its commit, the command or tool call, and what you expected.

## Build

KnowCode is Rust; `rust-toolchain.toml` pins the compiler. Python 3.12 runs the checks and evaluation scripts, with
the standard library only.

```sh
cargo build --release --locked      # target/release/knowcode
```

Put the binary on your `PATH` to try it, or run it from `target/`. [Architecture](docs/architecture.md) explains the
crates and how the index works.

## Checks

CI runs these on every pull request. Run them before you open one:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/check-docs.py target/debug/knowcode   # docs match the binary's commands, flags, keys and tools
python3 scripts/check-doc-paths.py                    # docs name only files that exist
python3 scripts/check-first-run.py target/debug/knowcode
python3 -m unittest discover -s scripts/eval -p 'test_*.py'
scripts/check-size.sh                                 # the release binary against size-budget.txt
```

A change that alters answers should say how. [Evaluation](docs/evaluation.md#gates-per-change) describes snapshotting
the public query set before and after a change and explaining every difference.

## Changes

- Keep a pull request to one change, with a test that fails without it.
- Add a line to the "Unreleased" section of [CHANGELOG.md](CHANGELOG.md) for anything a user would notice.
- Docs are plain and concrete. When you add or rename a command, flag, config key or tool, update
  [usage](docs/usage.md) or [configuration](docs/configuration.md) and the `kc_help` topic in
  `crates/knowcode/src/help.rs`; `check-docs.py` fails until they agree.
- Claims about accuracy or cost need a measurement on public repositories, with its counts, published alongside.
- Use only public repositories in examples, tests and fixtures, and made-up names for anything invented.

## The Claude Code plugin

The plugin's source is in [integrations/claude-code](integrations/claude-code/README.md); `knowcode setup claude`
installs the copy bundled into the binary. To check it:

```sh
claude plugin validate integrations/claude-code --strict
integrations/claude-code/test-hook.sh /path/to/indexed/repo SymbolName
```

## License

Contributions are licensed under MIT OR Apache-2.0, as the project is.
