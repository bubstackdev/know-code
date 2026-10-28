# Swift parse coverage

0.6's item "Swift parse coverage" ([plan](../docs/plans/0.6.md#1-identity-and-coverage)): the constructs behind most
parse errors in public Swift repositories, ranked, and the top ones read.

## The corpus

18 public Swift repositories at the commits the identity and workspace gates pin (identity-v1's
[repos.txt](../scripts/eval/identity/repos.txt) and [workspace-gate/repos.txt](../scripts/workspace-gate/repos.txt)):
Alamofire, CryptoSwift, KeychainAccess, Kingfisher, Nimble, Quick, SnapKit, SwiftLint, SwiftyJSON,
swift-algorithms, swift-argument-parser, swift-collections, swift-composable-architecture, swift-dependencies,
swift-log, swift-nio, swift-numerics and vapor. That's 4,297 Swift files. public-v3's repositories are a holdout and
are left out.

## Method

The [parse_errors](../crates/kc-extract/examples/parse_errors.rs) example runs KnowCode's Swift extractor over every
`.swift` file, as indexing does, recovery included. Per file it prints the parse errors, their first sites and (with
`KC_NAMES`) the declarations read. [swift_parse_coverage.py](../scripts/eval/swift_parse_coverage.py) puts each
site down to a construct:

- **By the site:** the construct the lines around the site show.
- **By the file:** failing that, one the file shows. An error at a file's first line can come from far below; those
  three were found by blanking 20-line windows of the file until its errors fell.

A file's errors are split over its sites' constructs. No third-party code is built or run: tree-sitter parses the
text.

```sh
cargo run --release -p kc-extract --example parse_errors -- DIR ... > errors.jsonl   # in the clones' directory
python3 scripts/eval/swift_parse_coverage.py errors.jsonl --clones CLONES
```

## Ranked, on integration/0.6 at 2cbba82

95 of the 4,297 files have parse errors, 716 errors in all:

| Construct | Files | Errors |
| --- | ---: | ---: |
| not put down to one construct | 36 | 270.4 |
| `()` as a statement | 4 | 85.0 |
| `borrow`, `mutate`, `unsafeAddress` accessors, `borrowing get` | 14 | 62.3 |
| `@Sendable func` in a body | 1 | 59.0 |
| `consume` before an operand | 2 | 44.0 |
| `@_specialize(where …)` attributes | 5 | 41.0 |
| identifiers with `$` inside (`_$id`) | 5 | 40.0 |
| closures passed as `get:` and `set:` | 1 | 26.0 |
| macros attached to members (`@Controller`, `@Suite`) | 3 | 18.0 |
| `@unsafe` in a conformance list | 1 | 12.0 |
| documentation snippets (`.docc` tutorial code) | 10 | 10.0 |
| partial ranges in `case` patterns (`x...:`) | 3 | 10.0 |
| regex literals | 3 | 10.0 |
| `#error` and `#warning` | 8 | 8.0 |
| `unsafe` before `!`, a number or a string | 5 | 7.2 |
| `where` on a generic typealias | 3 | 7.0 |
| `nonisolated(nonsending)` | 2 | 4.0 |
| non-ASCII identifiers (`y²`) | 1 | 2.0 |

## Read now

The extractor's recovery ([`parse`](../crates/kc-extract/src/swift.rs)) already re-parses a file with errors from a
copy, with constructs the grammar doesn't read blanked (same length, so every position stays). The copy is kept when
less of the file is in error. That copy now also:

- **Accessors:** reads `borrow` and `unsafeAddress` as `get`, `mutate` and `unsafeMutableAddress` as `set`, and
  `borrowing get` as `get`. This applies where the word opens an accessor: first on its line or after `{`, before `{`,
  `}` or another accessor's word (`{ borrow mutate }`).
- **Attributes and directives:** blanks `@_specialize(…)` attributes on one line, and `#error(…)` and `#warning(…)` on
  one line.
- **Local functions:** blanks `@Sendable` before a `func`.
- **Empty statements:** blanks `()` when it is a statement: a line of its own, or a closure's only one (`{ () }`).
- **Operators:** blanks `consume` before an operand, as it already did `unsafe` and `sending`. An operand now also
  starts with `!`, a digit, `$` or `"` (`if unsafe !done`, `while unsafe 0 < n`).

The Swift extractor's version moves to 20, so Swift files are read again.

## After

57 files have parse errors, 278 errors in all (from 95 and 716: 61% fewer errors).

- **No file got worse:** no file has more errors, and no file without errors reads differently.
- **What the counts mean:** the 38 files with no error left account for 64 fewer declarations read in all, but those were
  error recovery's misreadings. Function locals came up as declarations (`bucket`, `slot`, `context`), and members
  were cut off from their types. In their place come the real ones: `_HashNode.merge`, `RigidDeque.subscript`,
  `HTTPHandler.channelRead`, and the 32 tests of one vapor suite, now under their suite's type.

| Construct | Files | Errors |
| --- | ---: | ---: |
| not put down to one construct | 24 | 130.2 |
| identifiers with `$` inside (`_$id`) | 5 | 40.0 |
| closures passed as `get:` and `set:` | 1 | 26.0 |
| macros attached to members (`@Controller`, `@Suite`) | 2 | 17.0 |
| `()` as a statement | 2 | 14.0 |
| documentation snippets (`.docc` tutorial code) | 10 | 10.0 |
| partial ranges in `case` patterns (`x...:`) | 3 | 10.0 |
| regex literals | 3 | 10.0 |
| `where` on a generic typealias | 3 | 7.0 |
| `borrow`, `mutate`, `unsafeAddress` accessors, `borrowing get` | 2 | 4.8 |
| `nonisolated(nonsending)` | 2 | 4.0 |
| `#error` and `#warning` | 2 | 2.0 |
| non-ASCII identifiers (`y²`) | 1 | 2.0 |
| `@Sendable func` in a body | 1 | 1.0 |

**Not read, by choice:**

- **`_$name` identifiers:** names like `_$id` and `_$willModify` (the Composable Architecture's macro support) would
  read only with their `$` changed. That would put declarations under names the code doesn't have.
- **Documentation snippets:** these are fragments of tutorials, not code.

**On the identity gate** (identity-v1's three Swift repositories, 2cbba82 → this change):

- **Precision:** `exact` precision 0.961 → 0.962, and no label is newly wrong.
- **Recall:** recall linked 0.539 → 0.553; recall among candidates 0.813 → 0.828.
- **Outside labels linked:** 34 → 37. swift-nio's `ByteBuffer-core.swift` now parses, and its private
  `lowerBound` and `upperBound` become the one declaration of each name. Uses of the standard library's
  `Range.lowerBound` then link to them as `unique_name`: the name-collision cause the identity work tracks, now
  reached by more code.
