//! Declared test targets: what a repo's manifests say runs its tests, with the tests in each and the command to run
//! them. SwiftPM test targets (`.testTarget` in `Package.swift`), .NET test projects (a project referencing a test
//! SDK or framework) and `package.json` test scripts (`test`, `test:…`). Read from the index and the manifests as
//! declared, never ranked: which tests matter for a change is `test_select`'s question, kept apart from this one.

use std::path::Path;

use anyhow::Result;
use kc_store::Store;
use rusqlite::params;
use serde::Serialize;

use crate::privacy::Privacy;

/// A target's tests listed at most; the rest are counted (`tests_total`).
pub const MAX_TESTS: usize = 200;

/// .NET packages and SDKs that make a project a test project.
const DOTNET_TEST_PACKAGES: &[&str] =
    &["Microsoft.NET.Test.Sdk", "MSTest.Sdk", "MSTest.TestFramework", "MSTest", "xunit", "xunit.v3", "xunit.core", "NUnit", "TUnit"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestCase {
    /// The test's own name, with its container: `Class.method`.
    pub name: String,
    pub file: String,
    pub line: u32,
    /// How to run this test alone.
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestTarget {
    /// `swiftpm`, `dotnet` or `npm`.
    pub kind: &'static str,
    /// The target, project or script name.
    pub name: String,
    /// The manifest declaring it, repo-relative, and the line.
    pub manifest: String,
    pub line: u32,
    /// The directory its tests live in (the package's, for a script).
    pub dir: String,
    /// How to run the whole target, from the repo's root.
    pub command: String,
    /// Its tests (symbols tagged `test`): for a SwiftPM target and a .NET project, under its directory; for a script,
    /// the test files under its package (`*.test.*`, `*.spec.*`, `__tests__/`), by file.
    pub tests: Vec<TestCase>,
    pub tests_total: usize,
}

/// Every declared test target of the repo at `root`, in manifest and line order.
pub fn list(root: &Path, store: &Store) -> Result<Vec<TestTarget>> {
    let privacy = Privacy::load(root);
    let mut out = swiftpm(store)?;
    out.extend(dotnet(store)?);
    out.extend(npm(root, store, &privacy)?);
    out.sort_by(|a, b| (&a.manifest, a.line, &a.name).cmp(&(&b.manifest, b.line, &b.name)));
    Ok(out)
}

/// The directory of a repo-relative file, `""` at the root.
fn dir_of(file: &str) -> String {
    file.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string())
}

/// `path` joined under `dir`, repo-relative.
fn under(dir: &str, path: &str) -> String {
    if dir.is_empty() { path.to_string() } else { format!("{dir}/{path}") }
}

/// `sh` quoting for a path or a name in a command.
fn quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./:@+=".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The functions and methods tagged `test` in files under `dir` (the whole repo for `""`): (container, name, file,
/// line), in file and line order, and how many there are.
/// A test function or method: container, name, file, line, and its C# namespace when the index knows it.
type Found = (Option<String>, String, String, u32, Option<String>);

fn tests_under(store: &Store, dir: &str) -> Result<(Vec<Found>, usize)> {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let mut stmt = store.conn().prepare(
        "SELECT s.container, s.name, f.path, s.start_line,
                CASE WHEN f.lang = 'csharp' THEN coalesce(
                  CASE WHEN json_valid(s.type_name) THEN json_extract(s.type_name, '$.ns') END,
                  CASE WHEN json_valid(p.type_name) THEN json_extract(p.type_name, '$.ns') END) END
           FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
          WHERE s.kind IN ('function', 'method') AND (',' || s.tags || ',') LIKE '%,test,%'
            AND substr(f.path, 1, length(?1)) = ?1
          ORDER BY f.path, s.start_line",
    )?;
    let rows: Vec<Found> =
        stmt.query_map(params![prefix], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let total = rows.len();
    Ok((rows.into_iter().take(MAX_TESTS).collect(), total))
}

/// SwiftPM test targets: `swift test --filter`, from the package's directory. A target's tests are under
/// `Tests/<target>/` (SwiftPM's default; a custom `path:` isn't read).
fn swiftpm(store: &Store) -> Result<Vec<TestTarget>> {
    let mut stmt = store.conn().prepare(
        "SELECT t.name, f.path, t.line FROM package_targets t JOIN files f ON f.id = t.file_id WHERE t.kind = 'testTarget'",
    )?;
    let targets: Vec<(String, String, u32)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (name, manifest, line) in targets {
        let pkg = dir_of(&manifest);
        let dir = under(&pkg, &format!("Tests/{name}"));
        let package_path = if pkg.is_empty() { String::new() } else { format!(" --package-path {}", quote(&pkg)) };
        let (rows, total) = tests_under(store, &dir)?;
        let tests = rows
            .into_iter()
            .map(|(container, test, file, line, _)| {
                // XCTest names a test `Target.Class/method`; Swift Testing `Target.Suite/Nested/function()`, a free
                // one `Target.function()`. `--filter` takes a regular expression over them.
                let filter = match container.as_deref() {
                    Some(c) => format!("^{name}\\.{}/{test}\\b", c.replace('.', "[./]")),
                    None => format!("^{name}\\.{test}\\b"),
                };
                TestCase {
                    name: container.as_ref().map_or(test.clone(), |c| format!("{c}.{test}")),
                    file,
                    line,
                    command: format!("swift test{package_path} --filter {}", quote(&filter)),
                }
            })
            .collect();
        out.push(TestTarget {
            kind: "swiftpm",
            command: format!("swift test{package_path} --filter {}", quote(&format!("^{name}\\."))),
            name,
            manifest,
            line,
            dir,
            tests,
            tests_total: total,
        });
    }
    Ok(out)
}

/// .NET test projects: a project file referencing a test SDK or framework. `dotnet test <project>`.
fn dotnet(store: &Store) -> Result<Vec<TestTarget>> {
    let marks: Vec<String> = DOTNET_TEST_PACKAGES.iter().map(|p| format!("'{}'", p.to_lowercase())).collect();
    let mut stmt = store.conn().prepare(&format!(
        "SELECT f.path, MIN(d.line) FROM package_deps d JOIN files f ON f.id = d.file_id
          WHERE f.lang = 'csproj' AND (lower(d.name) IN ({0}) OR lower(substr(d.name, 1, instr(d.name || '/', '/') - 1)) IN ({0}))
          GROUP BY f.path",
        marks.join(", ")
    ))?;
    let projects: Vec<(String, u32)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (manifest, line) in projects {
        let dir = dir_of(&manifest);
        let name = manifest.rsplit('/').next().unwrap_or(&manifest).trim_end_matches(".csproj").to_string();
        let (rows, total) = tests_under(store, &dir)?;
        let tests = rows
            .into_iter()
            .map(|(container, test, file, line, ns)| {
                let full = container.as_ref().map_or(test.clone(), |c| format!("{c}.{test}"));
                TestCase {
                    command: format!("dotnet test {} --filter {}", quote(&manifest), quote(&dotnet_filter(ns.as_deref(), container.as_deref(), &test))),
                    name: full,
                    file,
                    line,
                }
            })
            .collect();
        out.push(TestTarget {
            kind: "dotnet",
            command: format!("dotnet test {}", quote(&manifest)),
            name,
            manifest,
            line,
            dir,
            tests,
            tests_total: total,
        });
    }
    Ok(out)
}

/// `dotnet test --filter` for the one test method `name` of the class `container` in the namespace `ns`: its
/// `FullyQualifiedName` exactly (`=`; `~` would also run every test whose name starts with it), or that name followed by
/// arguments, as NUnit names a parameterized test's cases (`Ns.Class.Method(1,2)`). A nested class is written both ways
/// the adapters write it (`Outer.Inner`, `Outer+Inner`). Filter operators in a value are escaped with `\`.
fn dotnet_filter(ns: Option<&str>, container: Option<&str>, name: &str) -> String {
    let escape = |v: &str| v.chars().fold(String::new(), |mut out, c| {
        if matches!(c, '\\' | '(' | ')' | '&' | '|' | '=' | '!' | '~') {
            out.push('\\');
        }
        out.push(c);
        out
    });
    let ns = ns.filter(|n| !n.is_empty()).map_or(String::new(), |n| format!("{n}."));
    let mut classes = vec![container.unwrap_or_default().to_string()];
    if let Some(c) = container.filter(|c| c.contains('.')) {
        classes.push(c.replace('.', "+"));
    }
    classes
        .iter()
        .map(|c| escape(&if c.is_empty() { format!("{ns}{name}") } else { format!("{ns}{c}.{name}") }))
        .map(|full| format!("FullyQualifiedName={full}|FullyQualifiedName~{full}\\("))
        .collect::<Vec<_>>()
        .join("|")
}

/// The line of the key `name` in a `package.json`'s `scripts` object: the first line naming it after the line opening
/// `scripts`. 1 when it can't be told.
fn script_line(text: &str, name: &str) -> u32 {
    let key = format!("\"{name}\"");
    let Some(scripts) = text.find("\"scripts\"") else { return 1 };
    let Some(at) = text[scripts..].find(&key) else { return 1 };
    text[..scripts + at].matches('\n').count() as u32 + 1
}

/// Whether a repo-relative file is a JavaScript or TypeScript test file by its name.
fn is_js_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let js = [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e));
    js && !path.split('/').any(|c| c == "node_modules")
        && (name.contains(".test.") || name.contains(".spec.") || path.split('/').any(|c| c == "__tests__"))
}

/// The package manager a package's lockfile names, nearest first up to the repo's root: its command to run a script.
fn runner(root: &Path, pkg: &str) -> &'static str {
    let mut dir = pkg.to_string();
    loop {
        let at = if dir.is_empty() { root.to_path_buf() } else { root.join(&dir) };
        for (lock, run) in [("pnpm-lock.yaml", "pnpm run"), ("yarn.lock", "yarn"), ("bun.lockb", "bun run"), ("bun.lock", "bun run"), ("package-lock.json", "npm run")] {
            if at.join(lock).is_file() {
                return run;
            }
        }
        if dir.is_empty() {
            return "npm run";
        }
        dir = dir_of(&dir);
    }
}

/// `package.json` test scripts (`test`, `test:…`, `test-…`), read from the working tree (scripts aren't indexed).
fn npm(root: &Path, store: &Store, privacy: &Privacy) -> Result<Vec<TestTarget>> {
    let manifests: Vec<String> = store
        .conn()
        .prepare("SELECT path FROM files WHERE lang = 'npm' ORDER BY path")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let files: Vec<String> = store.conn().prepare("SELECT path FROM files ORDER BY path")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for manifest in &manifests {
        if privacy.blocks(manifest) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(manifest)) else { continue };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else { continue };
        let pkg = dir_of(manifest);
        let run = runner(root, &pkg);
        let cd = if pkg.is_empty() { String::new() } else { format!("cd {} && ", quote(&pkg)) };
        let prefix = if pkg.is_empty() { String::new() } else { format!("{pkg}/") };
        // Test files of this package: under its directory and not in a package nested in it.
        let nested: Vec<String> =
            manifests.iter().map(|m| dir_of(m)).filter(|d| d.len() > pkg.len() && d.starts_with(&prefix)).map(|d| format!("{d}/")).collect();
        let test_files: Vec<&String> = files
            .iter()
            .filter(|f| f.starts_with(&prefix) && is_js_test_file(f) && !privacy.blocks(f))
            .filter(|f| !nested.iter().any(|n| f.starts_with(n.as_str())))
            .collect();
        for (name, _) in scripts.iter().filter(|(k, v)| (*k == "test" || k.starts_with("test:") || k.starts_with("test-")) && v.is_string()) {
            let line = script_line(&text, name);
            let command = if run == "npm run" && name == "test" { format!("{cd}npm test") } else { format!("{cd}{run} {}", quote(name)) };
            out.push(TestTarget {
                kind: "npm",
                name: name.clone(),
                manifest: manifest.clone(),
                line,
                dir: pkg.clone(),
                tests: test_files
                    .iter()
                    .take(MAX_TESTS)
                    .map(|f| TestCase { name: (*f).clone(), file: (*f).clone(), line: 1, command: format!("{command} -- {}", quote(f.strip_prefix(&prefix).unwrap_or(f))) })
                    .collect(),
                tests_total: test_files.len(),
                command,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_test_files() {
        assert_eq!(quote("Tests/My Target"), "'Tests/My Target'");
        assert_eq!(quote("^Kit\\."), "'^Kit\\.'");
        assert_eq!(quote("src/a.test.ts"), "src/a.test.ts");
        assert!(is_js_test_file("web/src/cart.test.ts") && is_js_test_file("web/__tests__/cart.js"));
        assert!(!is_js_test_file("web/src/cart.ts") && !is_js_test_file("node_modules/x/a.test.js"));
    }

    #[test]
    fn a_dotnet_test_filter_names_one_test() {
        assert_eq!(dotnet_filter(Some("Shop.Tests"), Some("MathTests"), "Add"), r"FullyQualifiedName=Shop.Tests.MathTests.Add|FullyQualifiedName~Shop.Tests.MathTests.Add\(");
        assert_eq!(dotnet_filter(None, Some("MathTests"), "Add"), r"FullyQualifiedName=MathTests.Add|FullyQualifiedName~MathTests.Add\(");
        assert_eq!(
            dotnet_filter(Some("N"), Some("Outer.Inner"), "T"),
            r"FullyQualifiedName=N.Outer.Inner.T|FullyQualifiedName~N.Outer.Inner.T\(|FullyQualifiedName=N.Outer+Inner.T|FullyQualifiedName~N.Outer+Inner.T\("
        );
    }

    #[test]
    fn a_scripts_line_is_its_key_in_scripts() {
        let text = "{\n  \"name\": \"test\",\n  \"keywords\": [\"test\"],\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"test\": \"vitest\"\n  }\n}\n";
        assert_eq!(script_line(text, "test"), 6);
        assert_eq!(script_line(text, "build"), 5);
        assert_eq!(script_line("{}", "test"), 1);
    }
}
