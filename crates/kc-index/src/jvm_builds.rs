//! Maven and Gradle builds as the projects they build, read from the manifests the index holds (`pom.xml` and Gradle
//! scripts, read as text): which directory is a project, the build it belongs to, and the name its build tool takes it
//! by. `areas` lists the projects as declared modules, and `test_targets` runs each one's tests from its build's root.

use kc_store::manifests::PackageRow;

/// The directory of a repo-relative file, `""` at the root.
fn dir_of(file: &str) -> &str {
    file.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `path` under `dir`, repo-relative.
fn join(dir: &str, path: &str) -> String {
    if dir.is_empty() { path.to_string() } else { format!("{dir}/{path}") }
}

/// Whether `path` is `dir` or under it (everything is under the root, `""`).
fn at_or_under(dir: &str, path: &str) -> bool {
    dir.is_empty() || path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Whether a package row is a Gradle settings script's.
pub fn is_gradle_settings(p: &PackageRow) -> bool {
    p.kind == "gradle" && p.file.rsplit('/').next().is_some_and(|f| f.starts_with("settings."))
}

/// A repo's Gradle builds: each settings script's directory and the project directories it includes.
pub struct GradleBuilds(Vec<(String, Vec<String>)>);

impl GradleBuilds {
    pub fn new(packages: &[PackageRow]) -> GradleBuilds {
        let builds = packages
            .iter()
            .filter(|p| is_gradle_settings(p))
            .map(|p| {
                let base = dir_of(&p.file);
                let projects = p.targets.iter().filter(|t| t.kind == "module").map(|t| join(base, &t.name.replace(':', "/")));
                (base.to_string(), projects.collect())
            })
            .collect();
        GradleBuilds(builds)
    }

    /// The root directory of the build a build script in `dir` belongs to: the directory of the settings script
    /// nearest above it when that script includes it or is beside it, or `dir` itself when no settings script is above
    /// it (a build of one project). None when the nearest settings script doesn't include it: Gradle never builds it.
    pub fn build_of<'a>(&'a self, dir: &'a str) -> Option<&'a str> {
        match self.0.iter().filter(|(b, _)| at_or_under(b, dir)).max_by_key(|(b, _)| b.len()) {
            None => Some(dir),
            Some((b, projects)) => (b == dir || projects.iter().any(|p| p == dir)).then_some(b.as_str()),
        }
    }
}

/// A project a Maven or Gradle build builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// `maven` or `gradle`.
    pub kind: &'static str,
    /// The coordinates a Maven project is depended on by (`groupId:artifactId`); a Gradle project's name.
    pub name: String,
    /// The manifest declaring it (its `pom.xml` or build script, or the settings script including it) and the line.
    pub manifest: String,
    pub line: u32,
    /// Its directory, repo-relative.
    pub dir: String,
    /// The directory its build is run from: the root of the Maven reactor aggregating it (the outermost `pom.xml`
    /// listing it among its `<modules>`, through each level), or the Gradle build's settings script's.
    pub root: String,
    /// How its build tool names it from `root`: the Maven module's directory under the root (`""` for the root
    /// itself), or the Gradle project path (`:web:admin`, `:` for the root project).
    pub path: String,
}

/// `path` with `.` and `..` segments worked out; None when it climbs out of the repo.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Every Maven and Gradle project of the repo, in manifest order. A Gradle project is the directory of a build script
/// its build includes, or a project the settings script includes that has no build script of its own; a Maven project
/// is each `pom.xml`. One directory is one project of each tool, named by its own manifest first.
pub fn projects(packages: &[PackageRow]) -> Vec<Project> {
    let mut out: Vec<Project> = Vec::new();
    // Maven: each POM's reactor root, climbing through the aggregators listing it.
    let poms: Vec<&PackageRow> = packages.iter().filter(|p| p.kind == "maven").collect();
    let aggregator_of = |dir: &str| {
        poms.iter().map(|p| dir_of(&p.file)).find(|&parent| {
            parent != dir
                && poms.iter().filter(|p| dir_of(&p.file) == parent).flat_map(|p| &p.targets).any(|t| {
                    t.kind == "module" && normalize(&join(parent, &t.name)).as_deref() == Some(dir)
                })
        })
    };
    for p in &poms {
        let dir = dir_of(&p.file);
        let mut root = dir;
        // At most as many steps as there are POMs, so that a cycle of `<modules>` can't loop.
        for _ in 0..poms.len() {
            match aggregator_of(root) {
                Some(up) => root = up,
                None => break,
            }
        }
        let path = if root.is_empty() { dir.to_string() } else { dir.strip_prefix(root).unwrap_or(dir).trim_start_matches('/').to_string() };
        out.push(Project { kind: "maven", name: p.name.clone(), manifest: p.file.clone(), line: p.line, dir: dir.to_string(), root: root.to_string(), path });
    }
    // Gradle: build scripts first, as a project's own script names it, then the projects included with none.
    let builds = GradleBuilds::new(packages);
    let gradle_path = |root: &str, dir: &str| {
        let rel = if root.is_empty() { dir } else { dir.strip_prefix(root).unwrap_or(dir).trim_start_matches('/') };
        format!(":{}", rel.replace('/', ":"))
    };
    let settings_name = |dir: &str| packages.iter().find(|p| is_gradle_settings(p) && dir_of(&p.file) == dir).map(|p| p.name.clone());
    let mut gradle: Vec<Project> = Vec::new();
    for p in packages.iter().filter(|p| p.kind == "gradle" && !is_gradle_settings(p)) {
        let dir = dir_of(&p.file);
        let Some(root) = builds.build_of(dir) else { continue };
        if gradle.iter().any(|g| g.dir == dir) {
            continue;
        }
        // The root project is named by its build's `rootProject.name`, or its directory.
        let name = if p.name == "(workspace)" { settings_name(dir).unwrap_or_else(|| p.name.clone()) } else { p.name.clone() };
        gradle.push(Project { kind: "gradle", name, manifest: p.file.clone(), line: p.line, dir: dir.to_string(), root: root.to_string(), path: gradle_path(root, dir) });
    }
    for p in packages.iter().filter(|p| is_gradle_settings(p)) {
        let root = dir_of(&p.file);
        for t in p.targets.iter().filter(|t| t.kind == "module") {
            let dir = join(root, &t.name.replace(':', "/"));
            if !gradle.iter().any(|g| g.dir == dir) {
                gradle.push(Project { kind: "gradle", name: t.name.clone(), manifest: p.file.clone(), line: t.line, path: gradle_path(root, &dir), dir, root: root.to_string() });
            }
        }
    }
    out.extend(gradle);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kc_store::manifests::TargetRow;

    fn pkg(kind: &str, name: &str, file: &str, modules: &[&str]) -> PackageRow {
        PackageRow {
            kind: kind.into(),
            name: name.into(),
            file: file.into(),
            line: 1,
            targets: modules.iter().map(|m| TargetRow { name: m.to_string(), kind: "module".into(), line: 3, deps: Vec::new() }).collect(),
            deps: Vec::new(),
        }
    }

    fn summary(projects: &[Project]) -> Vec<(&str, &str, &str, &str)> {
        projects.iter().map(|p| (p.name.as_str(), p.dir.as_str(), p.root.as_str(), p.path.as_str())).collect()
    }

    #[test]
    fn a_maven_module_runs_from_its_reactor_root() {
        let packages = vec![
            pkg("maven", "org.example:shop-parent", "pom.xml", &["core", "./web"]),
            pkg("maven", "org.example:core", "core/pom.xml", &[]),
            pkg("maven", "org.example:web", "web/pom.xml", &["admin"]),
            pkg("maven", "org.example:admin", "web/admin/pom.xml", &[]),
            // A POM no aggregator lists (an example, or a build of its own).
            pkg("maven", "org.example:sample", "samples/one/pom.xml", &[]),
        ];
        assert_eq!(
            summary(&projects(&packages)),
            vec![
                ("org.example:shop-parent", "", "", ""),
                ("org.example:core", "core", "", "core"),
                ("org.example:web", "web", "", "web"),
                ("org.example:admin", "web/admin", "", "web/admin"),
                ("org.example:sample", "samples/one", "samples/one", ""),
            ]
        );
    }

    #[test]
    fn a_gradle_project_is_named_by_its_path_in_its_build() {
        let packages = vec![
            pkg("gradle", "(workspace)", "build.gradle.kts", &[]),
            pkg("gradle", "orchard", "settings.gradle.kts", &["core", "web:admin", "docs"]),
            pkg("gradle", "core", "core/build.gradle.kts", &[]),
            pkg("gradle", "admin", "web/admin/build.gradle", &[]),
            // Not included: Gradle never builds it.
            pkg("gradle", "scratch", "scratch/build.gradle", &[]),
            // A build of its own beside its own settings script.
            pkg("gradle", "fixture", "plugin/src/test/fixture/build.gradle", &[]),
            pkg("gradle", "fixture", "plugin/src/test/fixture/settings.gradle", &[]),
        ];
        assert_eq!(
            summary(&projects(&packages)),
            vec![
                ("orchard", "", "", ":"),
                ("core", "core", "", ":core"),
                ("admin", "web/admin", "", ":web:admin"),
                ("fixture", "plugin/src/test/fixture", "plugin/src/test/fixture", ":"),
                ("docs", "docs", "", ":docs"),
            ]
        );
        let builds = GradleBuilds::new(&packages);
        assert_eq!(builds.build_of("scratch"), None);
        assert_eq!(builds.build_of("web/admin"), Some(""));
    }

    #[test]
    fn a_path_climbing_out_of_the_repo_is_none() {
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(normalize("../x"), None);
    }
}
