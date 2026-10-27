//! Package manifests: SwiftPM `Package.swift`, XcodeGen `project.yml` and Cargo `Cargo.toml`, parsed into packages,
//! targets and products, and the dependencies between them.
//!
//! Only literal manifest content is understood. A `Package.swift` that builds its arrays in variables or loops
//! yields what is written inline.

use tree_sitter::{Node, Parser};
use yaml_rust2::{Yaml, YamlLoader};

use crate::{DepSource, FileFacts, PackageDep, PackageFact, TargetDep, TargetFact};

pub const VERSION: u32 = 4;

pub fn swiftpm(source: &str) -> FileFacts {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_swift::LANGUAGE.into()).expect("tree-sitter-swift ABI matches tree-sitter");
    let Some(tree) = parser.parse(source, None) else {
        return FileFacts { parse_errors: 1, ..Default::default() };
    };
    let mut facts = FileFacts::default();
    if let Some(call) = find_call(tree.root_node(), source, "Package") {
        let args = Args::of(call, source);
        let mut pkg = PackageFact {
            kind: "swiftpm",
            name: args.string("name").unwrap_or_default(),
            line: line(call),
            targets: Vec::new(),
            deps: Vec::new(),
        };
        for el in args.array("products") {
            if let Some((method, a)) = member_call(el, source) {
                pkg.targets.push(TargetFact {
                    name: a.string("name").unwrap_or_default(),
                    kind: format!("product:{method}"),
                    line: line(el),
                    deps: a.strings("targets").into_iter().map(|t| TargetDep::target(t, line(el))).collect(),
                });
            }
        }
        for el in args.array("dependencies") {
            if let Some(("package", a)) = member_call(el, source) {
                pkg.deps.push(swiftpm_dep(&a, line(el)));
            }
        }
        for el in args.array("targets") {
            if let Some((method, a)) = member_call(el, source) {
                let mut target = TargetFact {
                    name: a.string("name").unwrap_or_default(),
                    kind: method.to_string(),
                    line: line(el),
                    deps: Vec::new(),
                };
                for dep in a.array("dependencies") {
                    if let Some(name) = string_literal(dep, source) {
                        // A bare string names a target or a product with the same name.
                        target.deps.push(TargetDep { name, product: None, package: None, kind: "by_name", line: line(dep) });
                    } else if let Some((m, d)) = member_call(dep, source) {
                        let name = d.string("name").or_else(|| d.positional_string()).unwrap_or_default();
                        let (kind, package) = match m {
                            "product" => ("product", d.string("package")),
                            "target" => ("target", None),
                            _ => ("by_name", None),
                        };
                        let product = (kind == "product").then(|| name.clone());
                        target.deps.push(TargetDep { name, product, package, kind, line: line(dep) });
                    }
                }
                pkg.targets.push(target);
            }
        }
        facts.packages.push(pkg);
    }
    facts
}

fn swiftpm_dep(a: &Args, line: u32) -> PackageDep {
    if let Some(path) = a.string("path") {
        let name = a.string("name").unwrap_or_else(|| last_path_component(&path));
        return PackageDep { name, source: DepSource::Path, location: path, requirement: None, line, resolved_repo: None };
    }
    let url = a.string("url").unwrap_or_default();
    let name = a.string("name").unwrap_or_else(|| last_path_component(url.trim_end_matches(".git")));
    let requirement = ["from", "exact", "branch", "revision"]
        .iter()
        .find_map(|k| a.string(k).map(|v| format!("{k} {v}")))
        .or_else(|| a.raw_after_url.clone());
    PackageDep { name, source: DepSource::Url, location: url, requirement, line, resolved_repo: None }
}

pub fn xcodegen(source: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let Ok(docs) = YamlLoader::load_from_str(source) else {
        facts.parse_errors = 1;
        return facts;
    };
    let Some(doc) = docs.first() else { return facts };
    let finder = LineFinder::new(source);
    let mut pkg = PackageFact {
        kind: "xcodegen",
        name: doc["name"].as_str().unwrap_or_default().to_string(),
        line: finder.key("name", 0),
        targets: Vec::new(),
        deps: Vec::new(),
    };
    let packages_line = finder.key("packages", 0);
    if let Some(packages) = doc["packages"].as_hash() {
        for (name, spec) in packages {
            let Some(name) = name.as_str() else { continue };
            let line = finder.key(name, packages_line);
            let dep = if let Some(path) = spec["path"].as_str() {
                PackageDep { name: name.to_string(), source: DepSource::Path, location: path.to_string(), requirement: None, line, resolved_repo: None }
            } else {
                let requirement = ["from", "majorVersion", "minorVersion", "exactVersion", "version", "branch", "revision"]
                    .iter()
                    .find_map(|k| yaml_scalar(&spec[*k]).map(|v| format!("{k} {v}")));
                PackageDep {
                    name: name.to_string(),
                    source: DepSource::Url,
                    location: spec["url"].as_str().or_else(|| spec["github"].as_str()).unwrap_or_default().to_string(),
                    requirement,
                    line,
                    resolved_repo: None,
                }
            };
            pkg.deps.push(dep);
        }
    }
    let targets_line = finder.key("targets", 0);
    if let Some(targets) = doc["targets"].as_hash() {
        for (name, spec) in targets {
            let Some(name) = name.as_str() else { continue };
            let line = finder.key(name, targets_line);
            let mut target = TargetFact {
                name: name.to_string(),
                kind: spec["type"].as_str().unwrap_or("target").to_string(),
                line,
                deps: Vec::new(),
            };
            for dep in spec["dependencies"].as_vec().map(Vec::as_slice).unwrap_or_default() {
                let d = if let Some(p) = dep["package"].as_str() {
                    let product = dep["product"].as_str().map(str::to_string);
                    TargetDep { name: product.clone().unwrap_or_else(|| p.to_string()), product, package: Some(p.to_string()), kind: "product", line }
                } else if let Some(t) = dep["target"].as_str() {
                    TargetDep::target(t.to_string(), line)
                } else if let Some(s) = dep["sdk"].as_str().or_else(|| dep["framework"].as_str()) {
                    TargetDep { name: s.to_string(), product: None, package: None, kind: "sdk", line }
                } else {
                    continue;
                };
                target.deps.push(d);
            }
            pkg.targets.push(target);
        }
    }
    facts.packages.push(pkg);
    facts
}

pub fn cargo(source: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let Ok(doc) = source.parse::<toml::Table>() else {
        facts.parse_errors = 1;
        return facts;
    };
    let finder = LineFinder::new(source);
    let package = doc.get("package").and_then(|p| p.as_table());
    let workspace = doc.get("workspace").and_then(|w| w.as_table());
    let name = package
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| if workspace.is_some() { "(workspace)".into() } else { String::new() });
    let mut pkg = PackageFact {
        kind: "cargo",
        name,
        line: finder.section(if package.is_some() { "package" } else { "workspace" }),
        targets: Vec::new(),
        deps: Vec::new(),
    };
    if let Some(members) = workspace.and_then(|w| w.get("members")).and_then(|m| m.as_array()) {
        for m in members.iter().filter_map(|m| m.as_str()) {
            pkg.targets.push(TargetFact { name: m.to_string(), kind: "member".into(), line: finder.text(m), deps: Vec::new() });
        }
    }
    for (section, kind) in [("lib", "lib"), ("bin", "bin"), ("test", "test"), ("bench", "bench"), ("example", "example")] {
        let items: Vec<&toml::Table> = match doc.get(section) {
            Some(toml::Value::Table(t)) => vec![t],
            Some(toml::Value::Array(a)) => a.iter().filter_map(|v| v.as_table()).collect(),
            _ => Vec::new(),
        };
        for t in items {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or(&pkg.name).to_string();
            pkg.targets.push(TargetFact { name, kind: kind.into(), line: finder.section(section), deps: Vec::new() });
        }
    }
    let mut dep_tables: Vec<(&str, &toml::Table)> = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = doc.get(key).and_then(|d| d.as_table()) {
            dep_tables.push((key, t));
        }
    }
    if let Some(t) = workspace.and_then(|w| w.get("dependencies")).and_then(|d| d.as_table()) {
        dep_tables.push(("workspace.dependencies", t));
    }
    for (section, table) in dep_tables {
        let section_line = finder.section(section);
        for (name, spec) in table {
            let line = finder.key(name, section_line);
            let (source, location, requirement) = match spec {
                toml::Value::String(v) => (DepSource::Registry, String::new(), Some(v.clone())),
                toml::Value::Table(t) => {
                    let get = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
                    if let Some(p) = get("path") {
                        (DepSource::Path, p, get("version"))
                    } else if let Some(g) = get("git") {
                        (DepSource::Url, g, get("tag").or_else(|| get("branch")).or_else(|| get("rev")))
                    } else if t.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                        (DepSource::Workspace, String::new(), None)
                    } else {
                        (DepSource::Registry, String::new(), get("version"))
                    }
                }
                _ => continue,
            };
            let requirement = match (section, requirement) {
                ("dependencies" | "workspace.dependencies", r) => r,
                (s, Some(r)) => Some(format!("{r} ({s})")),
                (s, None) => Some(format!("({s})")),
            };
            pkg.deps.push(PackageDep { name: name.clone(), source, location, requirement, line, resolved_repo: None });
        }
    }
    facts.packages.push(pkg);
    facts
}

/// An MSBuild project (`.csproj`). The package is its `PackageId`, else `AssemblyName`, else the file name, as
/// `dotnet pack` decides. The assembly and `RootNamespace` are its modules. Only literal elements are read; MSBuild
/// properties (`$(…)`) and imported `.props` files are not evaluated.
pub fn csproj(source: &str, file_stem: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    if !source.contains("<Project") {
        facts.parse_errors = 1;
        return facts;
    }
    let prop = |name: &str| element_text(source, name).filter(|v| !v.contains("$("));
    let assembly = prop("AssemblyName").unwrap_or_else(|| file_stem.to_string());
    let sdk = attr(source, "Project", "Sdk").unwrap_or_default();
    let kind = if sdk.ends_with(".Web") {
        "web"
    } else if prop("OutputType").is_some_and(|o| o.eq_ignore_ascii_case("exe") || o.eq_ignore_ascii_case("winexe")) {
        "exe"
    } else {
        "library"
    };
    let mut pkg = PackageFact {
        kind: "csproj",
        name: prop("PackageId").unwrap_or_else(|| assembly.clone()),
        line: line_of(source, source.find("<Project").unwrap_or(0)),
        targets: vec![TargetFact { name: assembly.clone(), kind: kind.into(), line: 1, deps: Vec::new() }],
        deps: Vec::new(),
    };
    if let Some(ns) = prop("RootNamespace").filter(|ns| *ns != assembly) {
        pkg.targets.push(TargetFact { name: ns, kind: "namespace".into(), line: 1, deps: Vec::new() });
    }
    for (at, tag) in elements(source, &["ProjectReference", "PackageReference", "Compile", "Import"]) {
        let line = line_of(source, at);
        let compiled = |pattern: String, location: String| PackageDep {
            name: pattern,
            source: DepSource::Compile,
            location,
            requirement: None,
            line,
            resolved_repo: None,
        };
        if tag_name(tag) == "Import" {
            // A shared project (`<Import Project="..\Shared\Shared.projitems" Label="Shared" />`): the sources in its
            // directory are compiled into this project.
            if let Some(items) = attr(tag, "Import", "Project").filter(|p| p.ends_with(".projitems") && !p.contains("$(")) {
                let items = items.replace('\\', "/");
                let pattern = match items.rsplit_once('/') {
                    Some((dir, _)) => format!("{dir}/**"),
                    None => "**".to_string(),
                };
                pkg.deps.push(compiled(items, pattern));
            }
            continue;
        }
        let Some(include) = attr(tag, tag_name(tag), "Include") else { continue };
        if tag_name(tag) == "Compile" {
            // Sources linked from outside the project's directory (`<Compile Include="..\Shared\Clock.cs" />`); those
            // inside it are the project's by directory already.
            for item in include.split(';').map(|i| i.trim().replace('\\', "/")) {
                if item.starts_with("../") && !item.contains("$(") {
                    pkg.deps.push(compiled(item.clone(), item));
                }
            }
            continue;
        }
        if tag_name(tag) == "ProjectReference" {
            let location = include.replace('\\', "/");
            let name = location.rsplit('/').next().unwrap_or(&location).trim_end_matches(".csproj").to_string();
            pkg.deps.push(PackageDep { name, source: DepSource::Path, location, requirement: None, line, resolved_repo: None });
        } else {
            // `Version="…"` on the element, or a `<Version>` child.
            let version = attr(tag, "PackageReference", "Version").or_else(|| {
                (!tag.ends_with("/>")).then(|| {
                    let rest = &source[at..];
                    let end = rest.find("</PackageReference>").unwrap_or(rest.len());
                    element_text(&rest[..end], "Version")
                })?
            });
            pkg.deps.push(PackageDep { name: include, source: DepSource::Registry, location: String::new(), requirement: version, line, resolved_repo: None });
        }
    }
    // The namespaces every file of the project imports: the SDK's implicit usings, and `<Using>` items.
    let using = |name: String, location: String, line: u32| PackageDep { name, source: DepSource::Using, location, requirement: None, line, resolved_repo: None };
    if let Some(v) = element_text(source, "ImplicitUsings") {
        let line = line_of(source, source.find("<ImplicitUsings>").unwrap_or(0));
        let on = v.eq_ignore_ascii_case("enable") || v.eq_ignore_ascii_case("true");
        if on && implicit_usings(&sdk).is_some() {
            pkg.deps.push(using(sdk.clone(), "implicit".into(), line));
            for ns in feature_usings(&sdk, source) {
                pkg.deps.push(using(ns.to_string(), "include".into(), line));
            }
        } else if on || v.contains("$(") {
            // An SDK whose implicit list isn't known, or a property deciding: what the files import can't be read.
            pkg.deps.push(using(sdk.clone(), "unknown".into(), line));
            let construct = if on { format!("implicit usings of the SDK {sdk}") } else { format!("<ImplicitUsings>{v}</ImplicitUsings>") };
            facts.unsupported.push(crate::Unsupported { area: "imports", construct, line });
        }
    }
    for (at, tag) in elements(source, &["Using"]) {
        let line = line_of(source, at);
        let get = |a: &str| attr(tag, "Using", a).filter(|v| !v.contains("$("));
        if let Some(ns) = get("Remove") {
            pkg.deps.push(using(ns, "remove".into(), line));
        } else if let Some(ns) = get("Include") {
            let how = match (get("Alias"), get("Static")) {
                (Some(a), _) => format!("alias:{a}"),
                (None, Some(s)) if s.eq_ignore_ascii_case("true") => "static".into(),
                _ => "include".into(),
            };
            pkg.deps.push(using(ns, how, line));
        } else if let Some(raw) = attr(tag, "Using", "Include") {
            pkg.deps.push(using(String::new(), "unknown".into(), line));
            facts.unsupported.push(crate::Unsupported { area: "imports", construct: format!("<Using Include=\"{raw}\">"), line });
        }
    }
    facts.packages.push(pkg);
    facts
}

/// The namespaces an MSBuild SDK imports implicitly when `ImplicitUsings` is enabled (.NET 6 and later, as
/// documented for each SDK); `None` for an SDK whose list isn't known here.
pub fn implicit_usings(sdk: &str) -> Option<&'static [&'static str]> {
    const BASE: &[&str] =
        &["System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks"];
    const WEB: &[&str] = &[
        "System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks",
        "System.Net.Http.Json", "Microsoft.AspNetCore.Builder", "Microsoft.AspNetCore.Hosting", "Microsoft.AspNetCore.Http",
        "Microsoft.AspNetCore.Routing", "Microsoft.Extensions.Configuration", "Microsoft.Extensions.DependencyInjection",
        "Microsoft.Extensions.Hosting", "Microsoft.Extensions.Logging",
    ];
    const WORKER: &[&str] = &[
        "System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks",
        "Microsoft.Extensions.Configuration", "Microsoft.Extensions.DependencyInjection", "Microsoft.Extensions.Hosting",
        "Microsoft.Extensions.Logging",
    ];
    // MSTest.Sdk 3.4 and later: Microsoft.NET.Sdk's, and the test framework's namespace (its own targets through
    // 3.8, the MSTest.TestFramework package it references from 3.10). 3.2 and 3.3 add none.
    const MSTEST: &[&str] = &[
        "System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks",
        "Microsoft.VisualStudio.TestTools.UnitTesting",
    ];
    // Aspire.AppHost.Sdk (every version): Microsoft.NET.Sdk's, and the Aspire.Hosting.AppHost package's it references.
    const ASPIRE_APPHOST: &[&str] = &[
        "System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks",
        "Aspire.Hosting", "Aspire.Hosting.ApplicationModel",
    ];
    // `Microsoft.NET.Sdk/8.0.100`: a versioned SDK is the SDK.
    let (name, version) = sdk.split_once('/').map_or((sdk.trim(), None), |(n, v)| (n.trim(), Some(v.trim())));
    match name {
        "" | "Microsoft.NET.Sdk" | "Microsoft.NET.Sdk.Razor" => Some(BASE),
        "Microsoft.NET.Sdk.Web" => Some(WEB),
        "Microsoft.NET.Sdk.Worker" => Some(WORKER),
        // Unversioned, the version is pinned elsewhere (global.json): the later list, which can only add a name.
        "MSTest.Sdk" => {
            let mut parts = version.unwrap_or_default().split(['.', '-']).map(|p| p.parse::<u32>().ok());
            match (parts.next().flatten(), parts.next().flatten()) {
                (Some(major), Some(minor)) if (major, minor) < (3, 4) => Some(BASE),
                _ => Some(MSTEST),
            }
        }
        "Aspire.AppHost.Sdk" => Some(ASPIRE_APPHOST),
        _ => None,
    }
}

/// The namespaces an SDK's opt-in features import with `ImplicitUsings` (MSTest.Sdk's `EnableAspireTesting`,
/// `EnablePlaywright`), as the project's properties turn them on.
fn feature_usings(sdk: &str, source: &str) -> Vec<&'static str> {
    if sdk.split('/').next().map(str::trim) != Some("MSTest.Sdk") {
        return Vec::new();
    }
    let on = |p: &str| element_text(source, p).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let mut out = Vec::new();
    if on("EnableAspireTesting") {
        out.push("Aspire.Hosting.Testing");
    }
    if on("EnablePlaywright") {
        out.push("Microsoft.Playwright.MSTest");
    }
    out
}

/// An npm `package.json`: the package name is the module others import; `dependencies`, `peerDependencies` and
/// `devDependencies` are its dependencies (`file:` and `link:` ones by path).
pub fn package_json(source: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let Ok(serde_json::Value::Object(doc)) = serde_json::from_str::<serde_json::Value>(source) else {
        facts.parse_errors = 1;
        return facts;
    };
    let finder = LineFinder::new(source);
    let name = doc.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
    let line = finder.key("name", 0);
    let mut pkg = PackageFact { kind: "npm", name: name.clone(), line, targets: Vec::new(), deps: Vec::new() };
    if !name.is_empty() {
        pkg.targets.push(TargetFact { name, kind: "module".into(), line, deps: Vec::new() });
    }
    for section in ["dependencies", "peerDependencies", "devDependencies", "optionalDependencies"] {
        let Some(table) = doc.get(section).and_then(|d| d.as_object()) else { continue };
        let section_line = finder.key(section, 0);
        for (dep, spec) in table {
            let Some(spec) = spec.as_str() else { continue };
            let line = finder.key(dep, section_line);
            let (source, location, requirement) = match spec.split_once(':') {
                Some(("file" | "link", path)) => (DepSource::Path, path.to_string(), None),
                Some(("workspace", r)) => (DepSource::Workspace, String::new(), Some(r.to_string())),
                Some(("git" | "git+https" | "git+ssh" | "github" | "https" | "http", _)) => (DepSource::Url, spec.to_string(), None),
                _ => (DepSource::Registry, String::new(), Some(spec.to_string())),
            };
            let requirement = match (section, requirement) {
                ("dependencies", r) => r,
                (s, Some(r)) => Some(format!("{r} ({s})")),
                (s, None) => Some(format!("({s})")),
            };
            pkg.deps.push(PackageDep { name: dep.clone(), source, location, requirement, line, resolved_repo: None });
        }
    }
    // serde_json's map is sorted by key; report dependencies in file order.
    pkg.deps.sort_by_key(|d| d.line);
    facts.packages.push(pkg);
    facts
}

/// The trimmed text of the first `<name>…</name>` element.
fn element_text(src: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = src.find(&open)? + open.len();
    let end = src[start..].find(&format!("</{name}>"))? + start;
    Some(src[start..end].trim().to_string()).filter(|v| !v.is_empty())
}

/// Start tags (`<Name …>` or `<Name …/>`) of the given element names, with their byte offsets.
fn elements<'a>(src: &'a str, names: &[&str]) -> Vec<(usize, &'a str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = src[i..].find('<') {
        let at = i + off;
        let end = src[at..].find('>').map_or(src.len(), |e| at + e + 1);
        let tag = &src[at..end];
        if names.contains(&tag_name(tag)) {
            out.push((at, tag));
        }
        i = at + 1;
    }
    out
}

fn tag_name(tag: &str) -> &str {
    let t = tag.trim_start_matches('<');
    &t[..t.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(t.len())]
}

/// The value of `attr="…"` on the first `<element …>` start tag in `src`.
fn attr(src: &str, element: &str, attr: &str) -> Option<String> {
    let (_, tag) = elements(src, &[element]).into_iter().next()?;
    let key = format!("{attr}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

fn line_of(src: &str, byte: usize) -> u32 {
    src[..byte.min(src.len())].matches('\n').count() as u32 + 1
}

/// Finds 1-based lines of YAML keys and TOML sections by text, since the parsers drop positions.
struct LineFinder<'a> {
    lines: Vec<&'a str>,
}

impl<'a> LineFinder<'a> {
    fn new(source: &'a str) -> Self {
        LineFinder { lines: source.lines().collect() }
    }

    /// First line at or after `after` (1-based; 0 for the start) whose trimmed text starts with `key:` or `key =`.
    fn key(&self, key: &str, after: u32) -> u32 {
        let quoted = [key.to_string(), format!("\"{key}\""), format!("'{key}'")];
        self.lines
            .iter()
            .enumerate()
            .skip(after as usize)
            .find(|(_, l)| {
                let t = l.trim_start().trim_start_matches("- ");
                quoted.iter().any(|k| {
                    t.strip_prefix(k.as_str()).is_some_and(|rest| {
                        let rest = rest.trim_start();
                        rest.starts_with(':') || rest.starts_with('=') || rest.starts_with('.')
                    })
                })
            })
            .map_or(after.max(1), |(i, _)| i as u32 + 1)
    }

    fn section(&self, name: &str) -> u32 {
        let (a, b) = (format!("[{name}]"), format!("[[{name}]]"));
        self.lines.iter().position(|l| l.trim() == a || l.trim() == b).map_or(1, |i| i as u32 + 1)
    }

    fn text(&self, needle: &str) -> u32 {
        self.lines.iter().position(|l| l.contains(needle)).map_or(1, |i| i as u32 + 1)
    }
}

fn yaml_scalar(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) | Yaml::Real(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

fn last_path_component(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

fn line(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count()).filter_map(move |i| node.child(i))
}

/// The first `Name(...)` call in the tree.
fn find_call<'t>(node: Node<'t>, src: &str, name: &str) -> Option<Node<'t>> {
    if node.kind() == "call_expression"
        && let Some(callee) = node.child(0)
        && callee.kind() == "simple_identifier"
        && &src[callee.byte_range()] == name
    {
        return Some(node);
    }
    children(node).find_map(|c| find_call(c, src, name))
}

/// `.method(args)`: the method name and its arguments.
fn member_call<'t>(node: Node<'t>, src: &str) -> Option<(&'static str, Args<'t>)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let callee = node.child(0)?;
    let name = match callee.kind() {
        "prefix_expression" => &src[callee.child_by_field_name("target")?.byte_range()],
        "navigation_expression" => {
            &src[callee.child_by_field_name("suffix")?.child_by_field_name("suffix")?.byte_range()]
        }
        _ => return None,
    };
    let known: &[&'static str] = &[
        "library", "executable", "plugin", "package", "target", "executableTarget", "testTarget", "macro",
        "binaryTarget", "systemLibrary", "product", "byName",
    ];
    let method = known.iter().find(|k| **k == name)?;
    Some((method, Args::of(node, src)))
}

fn string_literal(node: Node, src: &str) -> Option<String> {
    (node.kind() == "line_string_literal").then(|| {
        children(node).filter(|c| c.kind() == "line_str_text").map(|c| &src[c.byte_range()]).collect::<String>()
    })
}

/// Labeled arguments of a call.
struct Args<'t> {
    labeled: Vec<(String, Node<'t>)>,
    positional: Vec<Node<'t>>,
    src: String,
    /// For `.package(url:, "1.0.0"..<"2.0.0")` style requirements: the raw text of the argument after `url`.
    raw_after_url: Option<String>,
}

impl<'t> Args<'t> {
    fn of(call: Node<'t>, src: &str) -> Args<'t> {
        let mut args = Args { labeled: Vec::new(), positional: Vec::new(), src: src.to_string(), raw_after_url: None };
        let Some(suffix) = children(call).find(|c| c.kind() == "call_suffix") else { return args };
        let Some(values) = children(suffix).find(|c| c.kind() == "value_arguments") else { return args };
        let mut after_url = false;
        for arg in children(values).filter(|c| c.kind() == "value_argument") {
            let value = arg.child_by_field_name("value");
            match (arg.child_by_field_name("name"), value) {
                (Some(label), Some(v)) => {
                    let label = src[label.byte_range()].to_string();
                    after_url = label == "url";
                    args.labeled.push((label, v));
                }
                (None, Some(v)) => {
                    if after_url {
                        args.raw_after_url = Some(src[v.byte_range()].split_whitespace().collect::<Vec<_>>().join(" "));
                        after_url = false;
                    }
                    args.positional.push(v);
                }
                _ => {}
            }
        }
        args
    }

    fn get(&self, label: &str) -> Option<Node<'t>> {
        self.labeled.iter().find(|(l, _)| l == label).map(|(_, n)| *n)
    }

    fn string(&self, label: &str) -> Option<String> {
        string_literal(self.get(label)?, &self.src)
    }

    fn positional_string(&self) -> Option<String> {
        string_literal(*self.positional.first()?, &self.src)
    }

    fn array(&self, label: &str) -> Vec<Node<'t>> {
        match self.get(label) {
            Some(a) if a.kind() == "array_literal" => children(a).filter(|c| c.is_named() && c.kind() != "comment").collect(),
            _ => Vec::new(),
        }
    }

    fn strings(&self, label: &str) -> Vec<String> {
        self.array(label).into_iter().filter_map(|n| string_literal(n, &self.src)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_swiftpm() {
        let f = swiftpm(
            r#"// swift-tools-version: 6.2
import PackageDescription
let package = Package(
  name: "Bubstack",
  products: [
    .library(name: "BubstackKit", targets: ["BubstackKit"]),
  ],
  dependencies: [
    .package(path: "../shared"),
    .package(url: "https://github.com/apple/swift-collections.git", from: "1.1.0"),
  ],
  targets: [
    .target(name: "BubstackKit", dependencies: [.product(name: "Collections", package: "swift-collections")]),
    .testTarget(name: "BubstackKitTests", dependencies: ["BubstackKit", .target(name: "Helpers")]),
  ]
)"#,
        );
        let p = &f.packages[0];
        assert_eq!((p.name.as_str(), p.line), ("Bubstack", 3));
        let targets: Vec<_> = p.targets.iter().map(|t| (t.name.as_str(), t.kind.as_str())).collect();
        assert_eq!(targets, [("BubstackKit", "product:library"), ("BubstackKit", "target"), ("BubstackKitTests", "testTarget")]);
        assert_eq!(p.deps[0].name, "shared");
        assert_eq!(p.deps[0].source, DepSource::Path);
        assert_eq!(p.deps[1].name, "swift-collections");
        assert_eq!(p.deps[1].requirement.as_deref(), Some("from 1.1.0"));
        let kit = &p.targets[1].deps[0];
        assert_eq!((kit.kind, kit.package.as_deref(), kit.product.as_deref()), ("product", Some("swift-collections"), Some("Collections")));
        let test_deps: Vec<_> = p.targets[2].deps.iter().map(|d| (d.name.as_str(), d.kind)).collect();
        assert_eq!(test_deps, [("BubstackKit", "by_name"), ("Helpers", "target")]);
    }

    #[test]
    fn parses_xcodegen() {
        let f = xcodegen(
            "name: BubsMail\npackages:\n  Bubstack:\n    path: ../bubstack\n  Remote:\n    url: https://x.test/r\n    from: 1.2.0\ntargets:\n  BubsMail:\n    type: application\n    dependencies:\n      - package: Bubstack\n        product: BubstackKit\n      - target: Widget\n      - sdk: StoreKit.framework\n",
        );
        let p = &f.packages[0];
        assert_eq!(p.name, "BubsMail");
        assert_eq!((p.deps[0].name.as_str(), p.deps[0].location.as_str(), p.deps[0].line), ("Bubstack", "../bubstack", 3));
        assert_eq!(p.deps[1].requirement.as_deref(), Some("from 1.2.0"));
        let t = &p.targets[0];
        assert_eq!((t.name.as_str(), t.kind.as_str(), t.line), ("BubsMail", "application", 9));
        let deps: Vec<_> = t.deps.iter().map(|d| (d.name.as_str(), d.kind, d.package.as_deref())).collect();
        assert_eq!(deps, [("BubstackKit", "product", Some("Bubstack")), ("Widget", "target", None), ("StoreKit.framework", "sdk", None)]);
    }

    #[test]
    fn parses_cargo() {
        let f = cargo(
            "[package]\nname = \"kc-store\"\n\n[dependencies]\nanyhow = \"1\"\nkc-extract = { path = \"../kc-extract\" }\nserde.workspace = true\n\n[dev-dependencies]\ntempfile = \"3\"\n",
        );
        let p = &f.packages[0];
        assert_eq!((p.name.as_str(), p.line), ("kc-store", 1));
        let deps: Vec<_> = p.deps.iter().map(|d| (d.name.as_str(), d.source, d.line)).collect();
        assert_eq!(
            deps,
            [
                ("anyhow", DepSource::Registry, 5),
                ("kc-extract", DepSource::Path, 6),
                ("serde", DepSource::Workspace, 7),
                ("tempfile", DepSource::Registry, 10)
            ]
        );
        assert_eq!(p.deps[3].requirement.as_deref(), Some("3 (dev-dependencies)"));
    }

    #[test]
    fn csproj_packages_project_and_package_references() {
        let f = csproj(
            r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <AssemblyName>Acme.Cart.Api</AssemblyName>
    <PackageId>Acme.Cart</PackageId>
    <RootNamespace>Acme.Cart</RootNamespace>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\Acme.Cart.Service\Acme.Cart.Service.csproj" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Acme.Common.Data" Version="13.*" />
    <PackageReference Include="Newtonsoft.Json">
      <Version>13.0.3</Version>
    </PackageReference>
  </ItemGroup>
</Project>
"#,
            "Acme.Cart.Api",
        );
        assert_eq!(f.parse_errors, 0);
        let p = &f.packages[0];
        assert_eq!((p.kind, p.name.as_str(), p.line), ("csproj", "Acme.Cart", 1));
        // The assembly and root namespace are the modules other code imports.
        let targets: Vec<(&str, &str)> = p.targets.iter().map(|t| (t.name.as_str(), t.kind.as_str())).collect();
        assert_eq!(targets, [("Acme.Cart.Api", "web"), ("Acme.Cart", "namespace")]);
        let deps: Vec<(&str, DepSource, &str, Option<&str>, u32)> = p
            .deps
            .iter()
            .map(|d| (d.name.as_str(), d.source, d.location.as_str(), d.requirement.as_deref(), d.line))
            .collect();
        assert_eq!(
            deps,
            [
                ("Acme.Cart.Service", DepSource::Path, "../Acme.Cart.Service/Acme.Cart.Service.csproj", None, 9),
                ("Acme.Common.Data", DepSource::Registry, "", Some("13.*"), 12),
                ("Newtonsoft.Json", DepSource::Registry, "", Some("13.0.3"), 13),
            ]
        );
    }

    #[test]
    fn csproj_links_sources_from_outside_its_directory() {
        let f = csproj(
            r#"<Project ToolsVersion="15.0">
  <ItemGroup>
    <Compile Include="Local.cs" />
    <Compile Include="..\Shared\Clock.cs;..\Shared\Time\**\*.cs">
      <Link>Clock.cs</Link>
    </Compile>
    <Compile Include="$(Root)\Gen.cs" />
  </ItemGroup>
  <Import Project="..\Common\Common.projitems" Label="Shared" />
  <Import Project="$(MSBuildToolsPath)\Microsoft.CSharp.targets" />
</Project>
"#,
            "App",
        );
        let deps: Vec<(&str, DepSource, &str)> = f.packages[0].deps.iter().map(|d| (d.name.as_str(), d.source, d.location.as_str())).collect();
        assert_eq!(
            deps,
            [
                ("../Shared/Clock.cs", DepSource::Compile, "../Shared/Clock.cs"),
                ("../Shared/Time/**/*.cs", DepSource::Compile, "../Shared/Time/**/*.cs"),
                ("../Common/Common.projitems", DepSource::Compile, "../Common/**"),
            ]
        );
    }

    #[test]
    fn csproj_defaults_to_the_file_name() {
        let f = csproj("<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><OutputType>Exe</OutputType></PropertyGroup>\n</Project>\n", "Tool");
        let p = &f.packages[0];
        assert_eq!(p.name, "Tool");
        assert_eq!(p.targets.iter().map(|t| (t.name.as_str(), t.kind.as_str())).collect::<Vec<_>>(), [("Tool", "exe")]);
    }

    #[test]
    fn package_json_names_and_dependencies() {
        let f = package_json(
            r#"{
  "name": "@acme/cart",
  "version": "1.2.0",
  "dependencies": {
    "@acme/foundation": "^4.1.0",
    "rxjs": "~7.8.0",
    "local-lib": "file:../local-lib"
  },
  "peerDependencies": { "@angular/core": "^18.0.0" },
  "devDependencies": {
    "typescript": "5.4.0"
  }
}"#,
        );
        assert_eq!(f.parse_errors, 0);
        let p = &f.packages[0];
        assert_eq!((p.kind, p.name.as_str(), p.line), ("npm", "@acme/cart", 2));
        assert_eq!(p.targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["@acme/cart"]);
        let deps: Vec<(&str, DepSource, &str, Option<&str>, u32)> = p
            .deps
            .iter()
            .map(|d| (d.name.as_str(), d.source, d.location.as_str(), d.requirement.as_deref(), d.line))
            .collect();
        assert_eq!(
            deps,
            [
                ("@acme/foundation", DepSource::Registry, "", Some("^4.1.0"), 5),
                ("rxjs", DepSource::Registry, "", Some("~7.8.0"), 6),
                ("local-lib", DepSource::Path, "../local-lib", None, 7),
                ("@angular/core", DepSource::Registry, "", Some("^18.0.0 (peerDependencies)"), 9),
                ("typescript", DepSource::Registry, "", Some("5.4.0 (devDependencies)"), 11),
            ]
        );
        assert_eq!(package_json("{ not json").parse_errors, 1);
    }

    /// MSTest.Sdk and Aspire.AppHost.Sdk import what their published targets and the packages they reference say
    /// (MSTest.Sdk 3.4+ and MSTest.TestFramework 3.10+; Aspire.Hosting.AppHost 8.0 to 13.6); an SDK not listed stays
    /// unread.
    #[test]
    fn test_and_app_host_sdks_have_known_implicit_usings() {
        let has = |sdk: &str, ns: &str| implicit_usings(sdk).is_some_and(|l| l.contains(&ns));
        assert!(has("MSTest.Sdk", "Microsoft.VisualStudio.TestTools.UnitTesting") && has("MSTest.Sdk", "System.Linq"));
        assert!(has("MSTest.Sdk/4.0.2", "Microsoft.VisualStudio.TestTools.UnitTesting"));
        assert!(has("MSTest.Sdk/3.4.0", "Microsoft.VisualStudio.TestTools.UnitTesting"));
        assert!(!has("MSTest.Sdk/3.3.1", "Microsoft.VisualStudio.TestTools.UnitTesting") && has("MSTest.Sdk/3.3.1", "System"));
        assert!(has("Aspire.AppHost.Sdk/13.6.0", "Aspire.Hosting") && has("Aspire.AppHost.Sdk", "Aspire.Hosting.ApplicationModel"));
        assert!(has("Aspire.AppHost.Sdk/9.0.0", "System.Threading.Tasks"));
        assert!(implicit_usings("Some.Other.Sdk/1.0.0").is_none());
        let project = |sdk: &str, props: &str| {
            let f = csproj(&format!("<Project Sdk=\"{sdk}\">\n  <PropertyGroup>\n    <ImplicitUsings>enable</ImplicitUsings>{props}\n  </PropertyGroup>\n</Project>\n"), "App");
            let usings: Vec<(String, String)> =
                f.packages[0].deps.iter().filter(|d| d.source == DepSource::Using).map(|d| (d.name.clone(), d.location.clone())).collect();
            (usings, f.unsupported.len())
        };
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(project("MSTest.Sdk", ""), (vec![s("MSTest.Sdk", "implicit")], 0));
        assert_eq!(
            project("MSTest.Sdk/4.0.2", "<EnableAspireTesting>true</EnableAspireTesting>"),
            (vec![s("MSTest.Sdk/4.0.2", "implicit"), s("Aspire.Hosting.Testing", "include")], 0)
        );
        assert_eq!(project("Aspire.AppHost.Sdk/13.6.0", ""), (vec![s("Aspire.AppHost.Sdk/13.6.0", "implicit")], 0));
        // Unknown: the files' imports can't be read, and that is said.
        assert_eq!(project("Some.Other.Sdk/1.0.0", ""), (vec![s("Some.Other.Sdk/1.0.0", "unknown")], 1));
    }
}
