# Grammar sizes

What each tree-sitter grammar adds to the dist binary when it is linked alone, measured before any grammar is linked
into KnowCode (0.7 plan, gate item "Grammar sizes"; decision D11: every grammar ships inside the binary). The figures
set the size-budget raises: each extractor PR that links a grammar cites its row here, plus its own dist build.

This first version holds the three grammars 0.7 links. The other 32 candidates (0.11's full languages and the tags
tier) are being measured the same way; their rows, ranked, and the alternatives for the largest follow in a later
commit. Nothing here is linked by this commit.

## Method

- **Build.** One throwaway commit, `8de0bad` on branch `w1/grammar-size-probe` (never merged), on top of
  integration/0.7 at `f2e38e3`. It gives the `knowcode` crate one optional feature per candidate grammar
  (`g-go`, `g-java`, …). With a feature on, `main` reads every language the grammar's crate exports when
  `KC_GRAMMAR_PROBE` is set, so link-time optimization keeps the whole grammar; with none on, the same code runs over an
  empty list. A grammar's figure is the dist binary's size with its feature on, minus the size with none.
- **Profile and toolchain.** `cargo build --locked --profile dist -p knowcode --features g-NAME` (fat LTO, one codegen
  unit, stripped), rustc 1.98.0 (88d9e12ae 2026-08-18), aarch64-apple-darwin. Sizes are the file's bytes, as
  `scripts/check-size.sh` reads them.
- **Reproduction.** Two passes, each in its own target directory and each with its own base build: pass A in the
  candidates' order, pass B in reverse. A figure is accepted only when the passes agree within 1%. Both passes'
  bases are 37,901,504 bytes.
- **Grammar crates.** Each depends only on `tree-sitter-language` and loads under tree-sitter 0.27.1. Each crate's
  `build.rs` was read before it was built: they compile the grammar's C sources and nothing else.

## Results

| Grammar | Crate | Node kinds | Parse states (large) | Bytes added, pass A | Pass B | A/B difference |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Kotlin | `tree-sitter-kotlin-ng` 1.1.0 | 289 | 11,432 (4,175) | 3,467,728 | 3,467,728 | 0.00% |
| Java | `tree-sitter-java` 0.23.5 | 321 | 1,385 (406) | 412,880 | 412,880 | 0.00% |
| Go | `tree-sitter-go` 0.25.0 | 219 | 1,442 (29) | 231,248 | 231,248 | 0.00% |

Ranked by bytes added. The three come to 4,111,856 bytes (4.11 MB) when added one at a time; their combined build is
in the full table.

What a grammar costs follows its parse table more than its node kinds. `parser.c` stores the first `LARGE_STATE_COUNT`
states as a dense table, a row of every symbol for each state, and the rest compressed. Kotlin has 4,175 large states
against Go's 29, so it costs fifteen times as much with a third more node kinds.

The Kotlin crate is `tree-sitter-kotlin-ng`, the tree-sitter-grammars organisation's continuation of the grammar,
because the crate published as `tree-sitter-kotlin` (0.3.8) requires tree-sitter older than 0.23.

## Remaining candidates (in progress)

PHP, Ruby, C, C++, Objective-C, Vue and Svelte (0.11's full languages), and for the tags tier Scala, Dart, Lua,
Elixir, Erlang, Haskell, OCaml, Zig, Bash, R, Julia, Perl, Clojure, Elm, Nix, Groovy, PowerShell, Fortran, Solidity,
F#, Gleam, Verilog, CUDA, MATLAB and Nim. Some single-pass figures are already far above the three here, Verilog's
most of all, so the full table also measures alternatives for the largest: tree-sitter-systemverilog for Verilog, and
CUDA parsed by the C++ grammar.
