//! References a toolchain binds outside the repo are never linked to a same-named repo declaration (identity-v1's
//! "no outside label linked"): a member reached through a value a package gives, a bare import of a module only
//! `node_modules` or the platform has, a global type a module doesn't import, a C# extension method a receiver can't
//! call, and a Swift `async` function no `await` covers.

use std::path::PathBuf;
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn repo(name: &str, files: &[(&str, &str)]) -> (PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!("knowcode-test-outside-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let mut store = Store::open(&dir.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&dir, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    (dir, store)
}

/// Each reference to `name` of `kinds` in `file`: its line, confidence and targets as `file:line`.
fn refs(store: &Store, name: &str, kinds: &[&str], file: &str) -> Vec<(u32, &'static str, Vec<String>)> {
    let (hits, _) = query::refs(store.conn(), name, kinds, 100).unwrap();
    hits.into_iter()
        .filter(|h| h.file == file)
        .map(|h| (h.line, h.confidence, h.targets.iter().map(|t| format!("{}:{}", t.file, t.line)).collect()))
        .collect()
}

fn linked(c: &str) -> bool {
    matches!(c, "exact" | "unique_name")
}

const PACKAGE_JSON: &str = r#"{ "name": "web", "dependencies": { "@acme/sdk": "1.0.0" } }"#;
const TSCONFIG: &str = r#"{ "compilerOptions": { "strict": true } }"#;

#[test]
fn a_member_of_a_value_a_package_gives_is_the_packages() {
    let (root, store) = repo(
        "ts-package-rooted",
        &[
            ("package.json", PACKAGE_JSON),
            ("tsconfig.json", TSCONFIG),
            ("src/legacy.d.ts", "export interface LegacyClient {\n    getItems(q: string): Promise<string[]>;\n}\n"),
            (
                "src/items.ts",
                "import { getItemsApi } from '@acme/sdk/items';\nimport { LegacyClient } from './legacy';\n\nexport async function load(api: unknown, legacy: LegacyClient) {\n    const a = await getItemsApi(api).getItems('x');\n    const b = (await getItemsApi(api!).getItems('y')).data;\n    const c = await legacy.getItems('z');\n    return [a, b, c];\n}\n",
            ),
        ],
    );
    let got = refs(&store, "getItems", &["call"], "src/items.ts");
    // Through the package's value: not linked to the declaration file's same-named member.
    for line in [5, 6] {
        assert!(got.iter().any(|g| g.0 == line && !linked(g.1)), "{got:?}");
    }
    // Through the repo's own type: still linked.
    assert!(got.iter().any(|g| g.0 == 7 && g.1 == "exact" && g.2 == ["src/legacy.d.ts:2"]), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_bare_import_is_never_placed_by_a_file_named_like_it() {
    let (root, store) = repo(
        "ts-bare-import",
        &[
            ("package.json", r#"{ "name": "web" }"#),
            ("tsconfig.json", TSCONFIG),
            ("src/utils/crypto.ts", "export function createHash(s: string) { return s; }\n"),
            ("src/utils/hash.test.ts", "import { createHash } from 'crypto';\n\nexport const h = createHash('md5');\n"),
            ("src/utils/view.ts", "import { createHash } from './crypto';\n\nexport const v = createHash('v');\n"),
        ],
    );
    let node = refs(&store, "createHash", &["call"], "src/utils/hash.test.ts");
    assert!(node.iter().all(|g| !linked(g.1)) && !node.is_empty(), "{node:?}");
    let own = refs(&store, "createHash", &["call"], "src/utils/view.ts");
    assert!(own.iter().any(|g| g.1 == "exact" && g.2 == ["src/utils/crypto.ts:1"]), "{own:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_module_that_imports_nothing_names_the_platforms_global_type() {
    let (root, store) = repo(
        "ts-global-type",
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/types.ts", "export interface Response { body: string }\n"),
            ("src/fetch.ts", "export async function parse(r: Response): Promise<string> {\n    return r.statusText;\n}\n"),
            ("src/legacy.ts", "function parseLegacy(r: Response) {\n    return r;\n}\n"),
        ],
    );
    let module = refs(&store, "Response", &["type_ref"], "src/fetch.ts");
    assert!(!module.is_empty() && module.iter().all(|g| !linked(g.1)), "{module:?}");
    // A script (no import or export) may name any of the repo's declarations, as before.
    let script = refs(&store, "Response", &["type_ref"], "src/legacy.ts");
    assert!(script.iter().any(|g| linked(g.1)), "{script:?}");
    let _ = std::fs::remove_dir_all(&root);
}

const CS_PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>\n";

#[test]
fn a_csharp_extension_method_applies_only_where_csharp_calls_it() {
    let (root, store) = repo(
        "cs-extensions",
        &[
            ("App/App.csproj", CS_PROJECT),
            (
                "App/Extensions.cs",
                "using System;\nusing System.Collections.Generic;\n\nnamespace App;\n\npublic static class Extensions\n{\n    public static int Count(this ReadOnlySpan<char> s, char c) => 0;\n    public static int IndexOf<T>(this IReadOnlyList<T> xs, T x) => 0;\n    public static void AddRange<T>(this ICollection<T> c, IEnumerable<T> xs) { }\n    public static bool Contains(this IEnumerable<string> xs, string x, StringComparison how) => false;\n    public static string Shout(this string s) => s;\n}\n",
            ),
            (
                "App/Use.cs",
                "using System;\nusing System.Collections.Generic;\n\nnamespace App;\n\npublic class Use\n{\n    private readonly List<string> _names = new();\n\n    public void Run(ReadOnlySpan<char> span, IReadOnlyList<string> results, string text)\n    {\n        var n = results.Count;\n        var i = span.IndexOf('$');\n        _names.AddRange(results);\n        var has = _names.Contains(text, StringComparison.Ordinal);\n        var s = text.Shout();\n    }\n}\n",
            ),
        ],
    );
    let file = "App/Use.cs";
    // A property read is never an extension method.
    let count = refs(&store, "Count", &["member", "call"], file);
    assert!(count.iter().all(|g| !linked(g.1)), "{count:?}");
    // `ReadOnlySpan<T>` implements no interface: an extension on `IReadOnlyList<T>` isn't one it can call.
    let index = refs(&store, "IndexOf", &["call"], file);
    assert!(!index.is_empty() && index.iter().all(|g| !linked(g.1)), "{index:?}");
    // `List<T>.AddRange` is an instance method, which C# calls before the extension.
    let add = refs(&store, "AddRange", &["call"], file);
    assert!(!add.is_empty() && add.iter().all(|g| !linked(g.1)), "{add:?}");
    // No instance `Contains` takes two arguments: the extension is called.
    let contains = refs(&store, "Contains", &["call"], file);
    assert!(contains.iter().any(|g| linked(g.1) && g.2 == ["App/Extensions.cs:11"]), "{contains:?}");
    // An extension on the receiver's own type is called.
    let shout = refs(&store, "Shout", &["call"], file);
    assert!(shout.iter().any(|g| linked(g.1) && g.2 == ["App/Extensions.cs:12"]), "{shout:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_swift_async_function_is_called_only_under_await() {
    let (root, store) = repo(
        "swift-unawaited",
        &[
            (
                "Tests/CoreTests/XCTest+Async.swift",
                "import XCTest\n\n@available(macOS 10.15, iOS 13.0, watchOS 6.0, tvOS 13.0, *)\n@available(swift, deprecated: 6.0, message: \"Use the throwing variant of this function, which takes the same arguments\")\ninternal func XCTAssertThrowsError<T>(_ expression: @autoclosure () async throws -> T, file: StaticString = #filePath, line: UInt = #line) async {\n}\n\nfunc retry<T>(_ body: () async throws -> T) reasync rethrows -> T {\n    try await body()\n}\n",
            ),
            (
                "Tests/CoreTests/CoreTests.swift",
                "import XCTest\n\nfinal class CoreTests: XCTestCase {\n    func testSync() throws {\n        XCTAssertThrowsError(try make())\n        let v = retry { 1 }\n        _ = v\n    }\n\n    func testAsync() async throws {\n        await XCTAssertThrowsError(try make())\n    }\n}\n\nfunc make() throws -> Int { 0 }\n",
            ),
        ],
    );
    let file = "Tests/CoreTests/CoreTests.swift";
    let assert = refs(&store, "XCTAssertThrowsError", &["call", "instantiate"], file);
    // Not awaited: XCTest's own synchronous function, not the repo's `async` overload.
    assert!(assert.iter().any(|g| g.0 == 5 && !linked(g.1)), "{assert:?}");
    assert!(assert.iter().any(|g| g.0 == 11 && linked(g.1) && g.2 == ["Tests/CoreTests/XCTest+Async.swift:3"]), "{assert:?}");
    // A `reasync` function needs no `await` with a synchronous closure.
    let retry = refs(&store, "retry", &["call"], file);
    assert!(retry.iter().any(|g| g.0 == 6 && linked(g.1)), "{retry:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_standard_library_member_through_an_unknown_receiver_is_listed_not_linked() {
    let (root, store) = repo(
        "stdlib-names",
        &[
            ("Sources/Wire/Header.swift", "struct Header {\n    static let size = 20\n    var startIndex: Int { 0 }\n}\n"),
            (
                "Sources/Wire/Read.swift",
                "func read<T>(_ s: String, _: T.Type) -> Int {\n    var n = 0\n    n &+= MemoryLayout<T>.size\n    let end = s.endIndex\n    _ = end\n    return n + Header.size\n}\n",
            ),
        ],
    );
    let file = "Sources/Wire/Read.swift";
    let size = refs(&store, "size", &["member"], file);
    // `MemoryLayout<T>.size` is the library's; `Header.size` the repo's.
    assert!(size.iter().any(|g| g.0 == 3 && !linked(g.1)), "{size:?}");
    assert!(size.iter().any(|g| g.0 == 6 && g.1 == "exact" && g.2 == ["Sources/Wire/Header.swift:2"]), "{size:?}");
    let end = refs(&store, "endIndex", &["member"], file);
    assert!(end.iter().all(|g| !linked(g.1)), "{end:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_csharp_member_of_a_property_a_library_base_class_may_declare_is_listed_not_linked() {
    let (root, store) = repo(
        "cs-inherited-property",
        &[
            ("Shop/Shop.csproj", CS_PROJECT),
            ("Shop/Basket.cs", "namespace Shop;\n\npublic class BasketForm\n{\n    public string QueryString { get; set; } = \"\";\n}\n"),
            (
                "Shop/BasketController.cs",
                "using Microsoft.AspNetCore.Mvc;\n\nnamespace Shop;\n\npublic class BasketController : ControllerBase\n{\n    public string Describe() => Request.QueryString.ToString();\n}\n",
            ),
        ],
    );
    let got = refs(&store, "QueryString", &["member"], "Shop/BasketController.cs");
    assert!(!got.is_empty() && got.iter().all(|g| !linked(g.1)), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_swift_type_a_module_imports_by_name_from_a_package_is_the_packages() {
    let (root, store) = repo(
        "swift-scoped-import",
        &[
            ("Sources/Core/Handle.swift", "public protocol Handle {\n    func close() throws\n}\n"),
            ("Sources/Files/Exports.swift", "@_exported import struct SystemKit.Handle\n"),
            (
                "Sources/Files/Open.swift",
                "import Core\nimport SystemKit\n\nfunc shut(_ handle: Handle) throws {\n    try handle.close()\n}\n",
            ),
            ("Sources/Other/Use.swift", "import Core\n\nfunc shut(_ handle: Handle) throws {\n    try handle.close()\n}\n"),
        ],
    );
    // In `Files`, `Handle` is SystemKit's, and so is its `close()`.
    let ty = refs(&store, "Handle", &["type_ref"], "Sources/Files/Open.swift");
    assert!(!ty.is_empty() && ty.iter().all(|g| !linked(g.1)), "{ty:?}");
    let close = refs(&store, "close", &["call"], "Sources/Files/Open.swift");
    assert!(!close.is_empty() && close.iter().all(|g| !linked(g.1)), "{close:?}");
    // Elsewhere it is the repo's.
    let other = refs(&store, "Handle", &["type_ref"], "Sources/Other/Use.swift");
    assert!(other.iter().any(|g| linked(g.1) && g.2 == ["Sources/Core/Handle.swift:1"]), "{other:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_swift_platform_shim_test_extension_or_library_overload_is_listed_not_linked() {
    let (root, store) = repo(
        "swift-shims",
        &[
            ("Sources/Sys/Windows.swift", "#if os(Windows)\nfunc unlink(_ path: UnsafePointer<CChar>) -> CInt {\n    0\n}\n#endif\n"),
            ("Sources/Sys/Remove.swift", "import Glibc\n\nfunc remove(_ path: UnsafePointer<CChar>) -> CInt {\n    unlink(path)\n}\n"),
            ("Tests/SysTests/Deque+Append.swift", "import DequeModule\n\nextension Deque {\n    mutating func pushAll(contentsOf s: [Element]) {}\n}\n"),
            ("Sources/Sys/Queue.swift", "import DequeModule\n\nfunc fill(_ q: inout Deque<Int>) {\n    q.pushAll(contentsOf: [1])\n}\n"),
            ("Tests/SysTests/Fill.swift", "import DequeModule\n\nfunc refill(_ q: inout Deque<Int>) {\n    q.pushAll(contentsOf: [1])\n}\n"),
            ("Sources/Sys/Decoder+Buffer.swift", "import Foundation\n\nextension JSONDecoder {\n    func decode<T: Decodable>(_ t: T.Type, from buffer: [UInt8]) throws -> T { fatalError() }\n}\n"),
            ("Sources/Sys/Load.swift", "import Foundation\n\nfunc load(_ data: Data) throws -> Int {\n    try JSONDecoder().decode(Int.self, from: data)\n}\n"),
        ],
    );
    let unlink = refs(&store, "unlink", &["call"], "Sources/Sys/Remove.swift");
    assert!(!unlink.is_empty() && unlink.iter().all(|g| !linked(g.1)), "{unlink:?}");
    // A test target's extension: not from `Sources/`, still from its own target.
    let from_sources = refs(&store, "pushAll", &["call"], "Sources/Sys/Queue.swift");
    assert!(!from_sources.is_empty() && from_sources.iter().all(|g| !linked(g.1)), "{from_sources:?}");
    let from_tests = refs(&store, "pushAll", &["call"], "Tests/SysTests/Fill.swift");
    assert!(from_tests.iter().any(|g| linked(g.1)), "{from_tests:?}");
    let decode = refs(&store, "decode", &["call"], "Sources/Sys/Load.swift");
    assert!(!decode.is_empty() && decode.iter().all(|g| !linked(g.1)), "{decode:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_destructured_prop_of_a_package_type_is_the_packages() {
    let (root, store) = repo(
        "ts-destructured-props",
        &[
            ("package.json", PACKAGE_JSON),
            ("tsconfig.json", TSCONFIG),
            ("src/filters.ts", "export interface Filters {\n    Status?: string;\n}\n"),
            (
                "src/props.ts",
                "import type { JobInfo } from '@acme/sdk/models';\nimport type { Shelf } from './shelf';\n\nexport type JobProps = {\n    job: JobInfo;\n    shelf: Shelf;\n};\n",
            ),
            ("src/shelf.ts", "export interface Shelf {\n    label: string;\n}\n"),
            (
                "src/JobRow.tsx",
                "import { JobProps } from './props';\n\nexport const JobRow = ({ job, shelf: s }: JobProps) => {\n    const status = job.LastResult.Status;\n    return s.label + status;\n};\n",
            ),
            ("src/loose.ts", "export function show(x: any) {\n    return x.job;\n}\n"),
        ],
    );
    // Through the package's `JobInfo`: not the repo's `Filters.Status`.
    let status = refs(&store, "Status", &["member"], "src/JobRow.tsx");
    assert!(!status.is_empty() && status.iter().all(|g| !linked(g.1)), "{status:?}");
    // Through the repo's `Shelf`, renamed in the pattern: its own member.
    let label = refs(&store, "label", &["member"], "src/JobRow.tsx");
    assert!(label.iter().any(|g| g.1 == "exact" && g.2 == ["src/shelf.ts:2"]), "{label:?}");
    // A type alias's property is reached through its type only, never by name from an untyped value.
    let job = refs(&store, "job", &["member"], "src/loose.ts");
    assert!(job.iter().all(|g| !linked(g.1)), "{job:?}");
    let _ = std::fs::remove_dir_all(&root);
}
