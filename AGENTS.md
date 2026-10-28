# Repository Guidelines

## Project Structure & Module Organization
KnowCode is a Rust 2024 workspace with four crates under `crates/`:
- `kc-extract`: per-file syntax and framework fact extraction.
- `kc-store`: SQLite storage, name resolution, and graph queries.
- `kc-index`: incremental indexing, freshness, configuration, and cross-repository linking.
- `knowcode`: CLI, MCP server, rendering, and client setup.

Rust integration tests live in each crate's `tests/` directory; unit tests accompany source modules. `scripts/` contains validation and evaluation tooling, `eval/` holds query sets and study artifacts, `docs/` explains architecture and contracts, and `integrations/` contains client integration assets.

## Build, Test, and Development Commands
Use the toolchain pinned in `rust-toolchain.toml`.
- `cargo build --release --locked`: build `target/release/knowcode`.
- `cargo run -p knowcode -- --help`: explore the local CLI.
- `cargo test --workspace --locked`: run Rust tests across all crates.
- `cargo test --workspace --locked -- invariant`: run the invariant suite separately.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: reject lint warnings.
- `cargo fmt --all -- --check`: check Rust formatting.
- `python3 -m unittest discover -s scripts/eval -p 'test_*.py'`: test the evaluation harness; repeat with `-s scripts/eval/neutral` for neutral comparisons.

## Coding Style & Naming Conventions
Follow rustfmt defaults, including four-space indentation. Use `snake_case` for modules and functions, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep extraction context-free and place orchestration in `kc-index`. Preserve answer completeness, confidence, evidence, and provenance contracts documented in `docs/answers.md`.

## Testing Guidelines
Use Rust's built-in test framework and Python `unittest`. Name property-oriented tests `invariant_*`; use descriptive behavior names elsewhere. Add regression coverage in the relevant crate. No numeric coverage threshold is documented. For answer changes, follow `docs/evaluation.md`: compare base and changed snapshots with separate fresh caches and explain every difference. CI checks Linux and macOS.

## Commit & Pull Request Guidelines
Recent commits use concise, descriptive subjects, sometimes prefixed with a subsystem (`setup codex:`); merged subjects commonly include a PR number. Describe the problem, resulting behavior, linked issue when applicable, and verification performed. Explain answer snapshot differences and update affected documentation.

## Agent & Process Safety
Never suspend, terminate, or change terminal state for another session's jobs. Control only processes started by the current session; reduce your workload or report contention when resources are shared.
