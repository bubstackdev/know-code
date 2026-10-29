# Grammar sizes

What each tree-sitter grammar adds to the dist binary when it is linked alone, measured before any grammar is linked
into KnowCode (0.7 plan, gate item "Grammar sizes"; decision D11: every grammar ships inside the binary). The figures
set the size-budget raises: each extractor PR that links a grammar cites its row here, plus its own dist build.

It covers 35 candidates: the three grammars 0.7 links, 0.11's full languages, and 25 for 0.11's tags tier. The
maintainer's decision on the full table, 2026-10-29: every grammar is bundled (D11), about 143.5 MB in all, in 0.11.

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

## All candidates

The 35 candidates, ranked by bytes added alone, measured as above in the same two passes. "Languages linked" counts the
languages a crate exports, every one of which the probe reads: a crate shipping several grammars (OCaml's
implementation, interface and type grammars; F# and its signature files; PHP with and without HTML) costs all of them.

| Rank | Grammar | For | Crate | Languages linked | Node kinds | Parse states (large) | Bytes added, pass A | Pass B | A/B |
| ---: | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | Verilog | tags tier | `tree-sitter-verilog` 1.0.3 | 1 | 1,223 | 20,534 (6,920) | 18,262,352 | 18,262,352 | 0.00% |
| 2 | F# | tags tier | `tree-sitter-fsharp` 0.3.12 | 2 | 543 + 416 | 21,942 (8,771) + 15,693 (530) | 14,233,776 | 14,233,776 | 0.00% |
| 3 | OCaml | tags tier | `tree-sitter-ocaml` 0.26.0 | 3 | 555 + 554 + 554 | 12,097 (2,690) + 13,556 (3,273) + 11,973 (2,587) | 12,912,928 | 12,912,928 | 0.00% |
| 4 | CUDA | tags tier | `tree-sitter-cuda` 0.21.2 | 1 | 594 | 13,077 (4,893) | 7,116,880 | 7,116,880 | 0.00% |
| 5 | Fortran | tags tier | `tree-sitter-fortran` 0.6.0 | 1 | 692 | 9,251 (4,137) | 6,638,032 | 6,638,032 | 0.00% |
| 6 | Julia | tags tier | `tree-sitter-julia` 0.23.1 | 1 | 245 | 19,618 (9,476) | 6,307,792 | 6,307,792 | 0.00% |
| 7 | Objective-C | 0.11 full | `tree-sitter-objc` 3.0.2 | 1 | 588 | 6,799 (4,121) | 5,366,480 | 5,366,480 | 0.00% |
| 8 | Perl | tags tier | `ts-parser-perl` 2.0.3 | 1 | 597 | 5,726 (3,640) | 4,755,712 | 4,755,712 | 0.00% |
| 9 | Scala | tags tier | `tree-sitter-scala` 0.26.3 | 1 | 462 | 18,280 (2,213) | 4,078,720 | 4,078,720 | 0.00% |
| 10 | Haskell | tags tier | `tree-sitter-haskell` 0.24.1 | 1 | 552 | 12,862 (2,378) | 3,897,120 | 3,897,120 | 0.00% |
| 11 | Kotlin | 0.7 | `tree-sitter-kotlin-ng` 1.1.0 | 1 | 289 | 11,432 (4,175) | 3,467,728 | 3,467,728 | 0.00% |
| 12 | C++ | 0.11 full | `tree-sitter-cpp` 0.23.4 | 1 | 543 | 8,637 (2,322) | 3,467,712 | 3,467,712 | 0.00% |
| 13 | Ruby | 0.11 full | `tree-sitter-ruby` 0.23.1 | 1 | 351 | 5,989 (2,168) | 2,130,304 | 2,130,304 | 0.00% |
| 14 | PHP | 0.11 full | `tree-sitter-php` 0.25.1 | 2 | 434 + 429 | 3,469 (850) + 3,385 (839) | 2,113,936 | 2,113,936 | 0.00% |
| 15 | Dart | tags tier | `tree-sitter-dart` 0.3.0 | 1 | 516 | 4,522 (875) | 1,469,776 | 1,469,776 | 0.00% |
| 16 | Elixir | tags tier | `tree-sitter-elixir` 0.3.5 | 1 | 235 | 7,001 (1,094) | 1,420,272 | 1,420,272 | 0.00% |
| 17 | Bash | tags tier | `tree-sitter-bash` 0.25.1 | 1 | 280 | 7,571 (523) | 1,387,232 | 1,387,232 | 0.00% |
| 18 | PowerShell | tags tier | `tree-sitter-powershell` 0.26.6 | 1 | 423 | 2,192 (510) | 1,023,952 | 1,023,952 | 0.00% |
| 19 | Groovy | tags tier | `tree-sitter-groovy` 0.1.2 | 1 | 348 | 2,928 (1,059) | 1,007,312 | 1,007,312 | 0.00% |
| 20 | Zig | tags tier | `tree-sitter-zig` 1.1.2 | 1 | 274 | 1,852 (942) | 693,584 | 693,584 | 0.00% |
| 21 | C | 0.11 full | `tree-sitter-c` 0.24.2 | 1 | 363 | 2,015 (455) | 627,536 | 627,536 | 0.00% |
| 22 | Solidity | tags tier | `tree-sitter-solidity` 1.2.13 | 1 | 531 | 977 (368) | 511,968 | 511,968 | 0.00% |
| 23 | R | tags tier | `tree-sitter-r` 1.3.0 | 1 | 136 | 2,112 (405) | 495,552 | 495,552 | 0.00% |
| 24 | Erlang | tags tier | `tree-sitter-erlang` 0.21.0 | 1 | 358 | 1,570 (290) | 446,032 | 446,032 | 0.00% |
| 25 | MATLAB | tags tier | `tree-sitter-matlab` 1.3.1 | 1 | 229 | 2,644 (326) | 429,520 | 429,520 | 0.00% |
| 26 | Java | 0.7 | `tree-sitter-java` 0.23.5 | 1 | 321 | 1,385 (406) | 412,880 | 412,880 | 0.00% |
| 27 | Gleam | tags tier | `tree-sitter-gleam` 1.0.0 | 1 | 275 | 1,967 (14) | 346,960 | 346,960 | 0.00% |
| 28 | Go | 0.7 | `tree-sitter-go` 0.25.0 | 1 | 219 | 1,442 (29) | 231,248 | 231,248 | 0.00% |
| 29 | Elm | tags tier | `tree-sitter-elm` 5.9.4 | 1 | 203 | 1,060 (20) | 181,856 | 181,856 | 0.00% |
| 30 | Clojure | tags tier | `tree-sitter-clojure-orchard` 0.2.8 | 1 | 73 | 496 (213) | 99,152 | 99,152 | 0.00% |
| 31 | Nix | tags tier | `tree-sitter-nix` 0.3.0 | 1 | 108 | 541 (2) | 82,768 | 82,768 | 0.00% |
| 32 | Lua | tags tier | `tree-sitter-lua` 0.5.0 | 1 | 143 | 282 (2) | 66,256 | 66,256 | 0.00% |
| 33 | Svelte | 0.11 full | `tree-sitter-svelte-ng` 1.0.2 | 1 | 118 | 345 (2) | 49,808 | 49,808 | 0.00% |
| 34 | Vue | 0.11 full | `tree-sitter-vue-next` 0.1.0 | 1 | 67 | 170 (2) | 33,296 | 33,296 | 0.00% |
| 35 | Nim (unrepresentative) | tags tier | `tree-sitter-nim` 0.1.0 | 1 | 58 | 105 (5) | 16,592 | 16,592 | 0.00% |

| Group | Grammars | Sum of single figures | MB |
| --- | ---: | ---: | ---: |
| 0.7 (Go, Java, Kotlin) | 3 | 4,111,856 | 4.11 |
| 0.11's full languages (PHP, Ruby, C, C++, Objective-C, Vue, Svelte) | 7 | 13,789,072 | 13.79 |
| Tags tier | 25 | 87,882,096 | 87.88 |
| All 35, sum of singles | 35 | 105,783,024 | 105.78 |

Linked together, in one build each pass: the three 0.7 grammars add 4,111,760 bytes (pass B 4,111,760), and all
35 add 105,632,720 (pass B 105,632,720), for a binary of 143,534,224 bytes. Linking grammars together saves almost
nothing over the sum of their single figures: each grammar's tables are its own.

Nim's figure is unrepresentative: the only Nim crate is a minimal grammar (58 node kinds, no external scanner), not the
full grammar a Nim tags tier would need.

The largest costs follow from dense parse tables: Verilog has 6,920 large states over 1,223 node kinds, Julia 9,476
and F#'s main grammar 8,771.

### CUDA through the C++ grammar

Measured as an alternative to bundling `tree-sitter-cuda` (7.1 MB) beside `tree-sitter-cpp`: every `.cu` and `.cuh`
file of two public CUDA repositories, parsed with both grammars, and the definitions each recovers (a function, class
or struct name and its line) compared.

| Repository | Files | Definitions, CUDA grammar | C++ grammar finds the same | Error-free files, C++ / CUDA | Bytes in error nodes, C++ / CUDA |
| --- | ---: | ---: | ---: | ---: | ---: |
| NVIDIA/cuda-samples (`5443602`) | 221 | 1,494 | 1,478 (98.9%) | 8.6% / 88.2% | 0.26% / 0.04% |
| NVIDIA/cutlass (`e13f5cd`) | 1,106 | 4,533 | 4,528 (99.9%) | 27.7% / 30.7% | 0.22% / 0.22% |

Most CUDA files have some error under the C++ grammar (kernel launches, `<<<…>>>`), but the errors cover a fraction of
a percent of the text, and the definitions a tags tier reads come through. The decision to bundle every grammar keeps
`tree-sitter-cuda` regardless; the figures are kept for 0.11.
