//! Declared test targets: SwiftPM test targets, .NET test projects and package.json test scripts, with their tests and
//! the commands to run them.

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
    write(&repo, "web/package.json", "{\n  \"name\": \"web\",\n  \"keywords\": [\"test\"],\n  \"scripts\": { \"build\": \"tsc\", \"test\": \"vitest run\", \"test:unit\": \"vitest run unit\" }\n}\n");
    write(&repo, "web/yarn.lock", "\n");
    write(&repo, "web/src/cart.ts", "export class Cart {}\n");
    write(&repo, "web/src/cart.test.ts", "import { Cart } from './cart';\n");
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
            ("npm", "test", "cd web && yarn test", vec!["web/src/cart.test.ts"]),
            ("npm", "test:unit", "cd web && yarn test:unit", vec!["web/src/cart.test.ts"]),
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
    // A script's line is its key under `scripts`, not an earlier `"test"`.
    assert_eq!((targets[2].manifest.as_str(), targets[2].line), ("web/package.json", 4));
    assert_eq!(targets[2].tests[0].command, "cd web && yarn test -- src/cart.test.ts");
    let _ = std::fs::remove_dir_all(&base);
}
