//! Declared test targets: SwiftPM test targets, .NET test projects, package.json test scripts and Go modules, with their
//! tests and the commands to run them.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};

fn base() -> PathBuf {
    let base = std::env::temp_dir().join(format!("knowcode-test-targets-{}", std::process::id()));
    // SAFETY: one test in this binary; set before anything reads them.
    unsafe {
        std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
        std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
        std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
    }
    base
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn declared_test_targets_list_their_tests_and_commands() {
    let base = base();
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("toolkit");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap().success());
    write(&repo, "Package.swift", r#"// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "Kit",
    targets: [
        .target(name: "Kit"),
        .testTarget(name: "KitTests", dependencies: ["Kit"]),
    ]
)
"#);
    write(&repo, "Sources/Kit/Kit.swift", "public struct Kit { public init() {} }\n");
    write(&repo, "Tests/KitTests/KitTests.swift", "import XCTest\n@testable import Kit\n\nfinal class KitTests: XCTestCase {\n    func testMakes() { _ = Kit() }\n    func helper() {}\n}\n");
    write(&repo, "dotnet/Ledger.Tests/Ledger.Tests.csproj", r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <PackageReference Include="Microsoft.NET.Test.Sdk" Version="17.10.0" />
    <PackageReference Include="xunit" Version="2.9.0" />
  </ItemGroup>
</Project>
"#);
    write(&repo, "dotnet/Ledger.Tests/BalanceTests.cs", "using Xunit;\nnamespace Ledger.Tests;\npublic class BalanceTests\n{\n    [Fact]\n    public void Adds() { }\n    public void Helper() { }\n}\n");
    write(&repo, "dotnet/Ledger/Ledger.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    // A Go module with a nested module: each module's tests are its own.
    write(&repo, "goapp/go.mod", "module example.com/orchard\n\ngo 1.22\n");
    write(&repo, "goapp/crate.go", "package orchard\n\nfunc Count() int { return 1 }\n");
    write(&repo, "goapp/crate_test.go", "package orchard\n\nimport \"testing\"\n\nfunc TestCount(t *testing.T) {}\n\nfunc BenchmarkCount(b *testing.B) {}\n\nfunc helper() {}\n");
    write(&repo, "goapp/press/press_test.go", "package press\n\nimport \"testing\"\n\nfunc TestJuice(t *testing.T) {}\n");
    write(&repo, "goapp/tools/go.mod", "module example.com/orchard/tools\n\ngo 1.22\n");
    write(&repo, "goapp/tools/gen_test.go", "package tools\n\nimport \"testing\"\n\nfunc TestGen(t *testing.T) {}\n");
    write(&repo, "web/package.json", "{\n  \"name\": \"web\",\n  \"keywords\": [\"test\"],\n  \"scripts\": { \"build\": \"tsc\", \"test\": \"vitest run\", \"test:unit\": \"vitest run unit\" }\n}\n");
    write(&repo, "web/yarn.lock", "\n");
    write(&repo, "web/src/cart.ts", "export class Cart {}\n");
    write(&repo, "web/src/cart.test.ts", "import { Cart } from './cart';\n");
    // A package nested in another: its test files are its own, never the outer package's too.
    write(&repo, "web/packages/ui/package.json", "{\n  \"name\": \"ui\",\n  \"scripts\": { \"test\": \"vitest run\" }\n}\n");
    write(&repo, "web/packages/ui/button.test.ts", "export {};\n");
    let root = kc_index::repo_root(&repo).unwrap();
    let mut store = kc_index::open(&root).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();

    let targets = kc_index::test_targets::list(&root, &store).unwrap();
    let summary: Vec<(&str, &str, &str, Vec<&str>)> =
        targets.iter().map(|t| (t.kind, t.name.as_str(), t.command.as_str(), t.tests.iter().map(|c| c.name.as_str()).collect())).collect();
    assert_eq!(
        summary,
        [
            ("swiftpm", "KitTests", r"swift test --filter '^KitTests\.'", vec!["KitTests.testMakes"]),
            ("dotnet", "Ledger.Tests", "dotnet test dotnet/Ledger.Tests/Ledger.Tests.csproj", vec!["BalanceTests.Adds"]),
            ("go", "example.com/orchard", "cd goapp && go test ./...", vec!["TestCount", "BenchmarkCount", "TestJuice"]),
            ("go", "example.com/orchard/tools", "cd goapp/tools && go test ./...", vec!["TestGen"]),
            ("npm", "test", "cd web && yarn test", vec!["web/src/cart.test.ts"]),
            ("npm", "test:unit", "cd web && yarn test:unit", vec!["web/src/cart.test.ts"]),
            ("npm", "test", "cd web/packages/ui && yarn test", vec!["web/packages/ui/button.test.ts"]),
        ],
        "{targets:#?}"
    );
    let swift = &targets[0].tests[0];
    assert_eq!((swift.file.as_str(), swift.line), ("Tests/KitTests/KitTests.swift", 5));
    assert_eq!(swift.command, r"swift test --filter '^KitTests\.KitTests/testMakes\b'");
    // Exactly the one test (with its namespace), not every test whose name starts with it.
    assert_eq!(
        targets[1].tests[0].command,
        r"dotnet test dotnet/Ledger.Tests/Ledger.Tests.csproj --filter 'FullyQualifiedName=Ledger.Tests.BalanceTests.Adds|FullyQualifiedName~Ledger.Tests.BalanceTests.Adds\('"
    );
    // One Go test, from its package's directory; a benchmark alone.
    let go: Vec<&str> = targets[2].tests.iter().map(|c| c.command.as_str()).collect();
    assert_eq!(
        go,
        ["cd goapp && go test . -run '^TestCount$'", "cd goapp && go test . -run '^$' -bench '^BenchmarkCount$'", "cd goapp && go test ./press -run '^TestJuice$'"]
    );
    assert_eq!((targets[2].manifest.as_str(), targets[2].line), ("goapp/go.mod", 1));
    // A script's line is its key under `scripts`, not an earlier `"test"`.
    assert_eq!((targets[4].manifest.as_str(), targets[4].line), ("web/package.json", 4));
    assert_eq!(targets[4].tests[0].command, "cd web && yarn test -- src/cart.test.ts");
    let _ = std::fs::remove_dir_all(&base);
}
