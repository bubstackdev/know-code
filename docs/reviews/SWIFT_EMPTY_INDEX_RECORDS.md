# Empty Swift IndexStore records

The real Atelier run and an owned two-file reproduction failed when one saved file was entirely excluded by conditional compilation. Its compiler record was valid but contained no symbols. The matching focused provider checks now qualify the narrow fix below; the original full-corpus failure remains retained.

LLVM's [record writer](https://github.com/swiftlang/llvm-project/blob/next/clang/lib/Index/IndexRecordWriter.cpp#L259-L272) writes declaration/occurrence blocks only when declarations exist. Adding an occurrence always registers its declaration. The [reader](https://github.com/swiftlang/llvm-project/blob/next/clang/lib/Index/IndexRecordReader.cpp#L191-L225) nevertheless visits its occurrence cursor unconditionally. The [public C API](https://github.com/swiftlang/llvm-project/blob/next/clang/tools/IndexStore/IndexStore.cpp#L515-L534) forwards that result. A false result is also possible for malformed data or callback interruption; it cannot generally mean an empty record.

The helper therefore first uses public `indexstore_record_reader_symbols_apply_f`, with caching disabled. A callback stops after the first symbol, after which ordinary full occurrence traversal remains required. Only successful enumeration with zero callbacks establishes an empty symbol table and avoids the absent occurrence cursor. Failed enumeration before any callback, factory failures, occurrence failures for nonempty records, and output failures remain fatal. Source/unit/record evidence stays present for excluded files; no declarations are invented.

The existing Swift provider test adds a fully excluded saved source and a tiny compiler-generated store. Its finite fault cases damage an empty record header and truncate empty/nonempty records, then verify repair. These cases qualify API behavior and specific corruption barriers; they do not certify arbitrary bit-level corruption detection. The upstream reader factory skips occurrence payloads during its initial container scan, and this change adds no private format parser, filename/hash heuristic, or general integrity validator.

Source authoring and syntax checks are separate from the integration owner's matching native/compiler results. The failed real run and initial reproduction must remain recorded as failures.

## Matching focused qualification

At source `747d0e7`, matching Debug producer `190ffd3a92a3473fa1b28a9dac6761ae`
(source `a53f99a`, with the subsequent test-only driver correction), both
`swift-provider` and `swiftpm-provider` pass. The standalone check preserves
all saved excluded-source bytes without inventing its excluded declaration,
reads a real healthy empty/nonempty store, rejects the three damaged-record
cases, and reads it again after repair. Existing compiler identity, UTF-8,
graph and SwiftPM saved-update checks also pass. Reader SHA-256:
`83f46a3c18af9fd182e0962cb7d0893afd7d062f80c8e655063ef84526f6c113`.

Evidence: `.artifacts/implementation-0.0.3/wave-01/empty-record-swift-providers-03/`
and `.artifacts/swift-provider/1794593799514617000/reader-records/`.
The initial new fixture failed because its frontend invocation rejected a
driver-only option; that failed attempt remains retained. The correction uses
the production driver's indexing invocation. These focused passes do not
certify the full Atelier corpus, an optimized artifact or distribution.
