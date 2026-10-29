//! A use inside a function or type declared in a function body is the enclosing function's, as a compiler's index
//! reports it: `impact` reaches that function in the same hop, not through the local helper.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, ImpactParams, Scope};

const FILES: &[(&str, &str)] = &[
    ("Package.swift", "// swift-tools-version:5.9\nimport PackageDescription\n\nlet package = Package(name: \"Gates\", targets: [.target(name: \"Gates\"), .testTarget(name: \"GatesTests\", dependencies: [\"Gates\"])])\n"),
    ("Sources/Gates/Gate.swift", "public final class Gate {\n    public init() {}\n\n    public func release() {}\n}\n"),
    (
        "Tests/GatesTests/GateTests.swift",
        "import XCTest\n@testable import Gates\n\nfinal class GateTests: XCTestCase {\n    func testReleaseInAHelper() {\n        func check(_ gate: Gate) {\n            gate.release()\n        }\n        check(Gate())\n    }\n\n    func testReleaseInALocalType() {\n        final class Holder {\n            let gate = Gate()\n            deinit {\n                gate.release()\n            }\n        }\n        _ = Holder()\n    }\n\n    func testSomethingElse() {\n        func check(_ n: Int) {}\n        check(1)\n    }\n}\n",
    ),
];

#[test]
fn a_use_in_a_local_helper_is_the_enclosing_functions() {
    let cache = test_cache();
    let base = cache.dir("local-decls");
    let root = repo(&base, "gates", FILES);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    let p = ImpactParams { symbol: Some("Sources/Gates/Gate.swift:4".into()), depth: 2, limit: 100, ..Default::default() };
    let out = ops::impact(&p, &scope).unwrap();
    let got: Vec<(String, u8)> = out.data[0].impact.affected.iter().map(|a| (a.qualified.clone(), a.hop)).collect();
    for want in ["GateTests.testReleaseInAHelper", "GateTests.testReleaseInALocalType"] {
        assert!(got.iter().any(|(q, h)| q == want && *h == 1), "{want} at hop 1 in {got:?}");
    }
    // Neither the helpers nor a test with a same-named helper that doesn't release.
    assert!(!got.iter().any(|(q, _)| q.ends_with("check") || q.contains("Holder") || q.ends_with("testSomethingElse")), "{got:?}");
    let _ = std::fs::remove_dir_all(&base);
}
