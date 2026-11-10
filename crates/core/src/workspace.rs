use crate::{
    Error, Result,
    protocol::AnalyzeRequest,
    update::{self, WorkerCommand},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub const EXCLUDED: &[&str] = &[
    ".git",
    ".research",
    ".artifacts",
    "node_modules",
    "obj",
    "bin",
    "target",
    ".build",
    ".worktrees",
    "DerivedData",
];
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub modified: u64,
    pub hash: String,
    #[serde(default)]
    pub changed: u64,
}
pub type Snapshot = BTreeMap<String, Stamp>;

fn swift_manifest(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name == "Package.swift"
                || (name.starts_with("Package@swift-") && name.ends_with(".swift"))
        })
}

fn source_language(path: &Path) -> Option<&'static str> {
    // A Swift package manifest is executable configuration, not module source.
    if swift_manifest(path) {
        return None;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts" | "tsx") => Some("typescript"),
        Some("cs") => Some("csharp"),
        Some("rs") => Some("rust"),
        Some("swift") => Some("swift"),
        _ => None,
    }
}

pub fn identity(root: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(root)?;
    let mut identity = format!("{}:{}", metadata.dev(), metadata.ino());
    if let Ok(git) = std::fs::symlink_metadata(root.join(".git")) {
        identity.push_str(&format!(":{}:{}", git.dev(), git.ino()));
        if git.is_file() {
            identity.push_str(&std::fs::read_to_string(root.join(".git"))?);
        }
    }
    Ok(format!("{:x}", Sha256::digest(identity.as_bytes())))
}

pub fn database_path(root: &Path, data: &Path) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
    data.join("workspaces").join(&hash[..24]).join("index.db")
}
pub fn ignored(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|p| {
        p.components().any(|c| {
            let name = c.as_os_str().to_string_lossy();
            EXCLUDED.contains(&name.as_ref()) || name.starts_with(".build-")
        })
    })
}
pub fn relevant(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "ts" | "tsx"
                | "cs"
                | "rs"
                | "swift"
                | "toml"
                | "pbxproj"
                | "xcworkspacedata"
                | "xcscheme"
                | "xcconfig"
                | "h"
                | "hpp"
                | "c"
                | "cc"
                | "cpp"
                | "m"
                | "mm"
                | "modulemap"
                | "metal"
                | "csproj"
                | "sln"
                | "slnx"
                | "slnf"
                | "props"
                | "targets"
                | "json"
                | "xml"
                | "txt"
                | "config"
                | "editorconfig"
                | "razor"
                | "resx"
        )
    ) || (path.file_name().is_some_and(|n| n == "config")
        && path
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == ".cargo")))
        || matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some(
                "yarn.lock"
                    | "pnpm-lock.yaml"
                    | "pnpm-workspace.yaml"
                    | ".pnpmfile.cjs"
                    | "pnpmfile.cjs"
                    | ".pnpmfile.mjs"
                    | "pnpmfile.mjs"
                    | "Cargo.lock"
                    | "rust-toolchain"
                    | "Package.resolved"
                    | ".npmrc"
                    | ".editorconfig"
                    | ".swift-version"
                    | ".tool-versions"
                    | ".xcode-version"
            )
        )
}
fn files_controlled(root: &Path, control: &crate::resource::Control) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Err(Error::Provider(format!(
            "workspace unavailable: {}",
            root.display()
        )));
    }
    let mut paths = vec![];
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !ignored(root, e.path()))
    {
        control.check()?;
        let entry = entry.map_err(|e| Error::Provider(e.to_string()))?;
        if entry.file_type().is_file() && relevant(entry.path()) {
            paths.push(entry.into_path());
        }
    }
    paths.sort();
    Ok(paths)
}
pub fn snapshot(
    root: &Path,
    previous: &Snapshot,
    force: &[PathBuf],
    extras: &[PathBuf],
) -> Result<Snapshot> {
    snapshot_controlled(
        root,
        previous,
        force,
        extras,
        &crate::resource::Control::default(),
    )
}
pub fn snapshot_controlled(
    root: &Path,
    previous: &Snapshot,
    force: &[PathBuf],
    extras: &[PathBuf],
    control: &crate::resource::Control,
) -> Result<Snapshot> {
    let mut paths = files_controlled(root, control)?;
    paths.extend_from_slice(extras);
    paths.sort();
    paths.dedup();
    let mut result = Snapshot::new();
    for path in paths {
        control.check()?;
        let key = path.to_string_lossy().into_owned();
        let metadata = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if extras.contains(&path) {
                    result.insert(
                        key,
                        Stamp {
                            size: 0,
                            modified: 0,
                            hash: "missing".into(),
                            changed: 0,
                        },
                    );
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let old = previous.get(&key);
        let changed = changed_time(&metadata);
        let hash = if !force.iter().any(|p| path == *p || path.starts_with(p))
            && old.is_some_and(|s| {
                s.size == metadata.len() && s.modified == modified && s.changed == changed
            }) {
            old.unwrap().hash.clone()
        } else {
            let mut file = std::fs::File::open(&path)?;
            let mut digest = Sha256::new();
            let mut bytes = [0; 65536];
            loop {
                control.check()?;
                let n = file.read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                digest.update(&bytes[..n]);
            }
            format!("{:x}", digest.finalize())
        };
        result.insert(
            key,
            Stamp {
                size: metadata.len(),
                modified,
                hash,
                changed,
            },
        );
    }
    Ok(result)
}
fn changed_time(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    (metadata.ctime().max(0) as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(metadata.ctime_nsec().max(0) as u64)
}

/// Final lightweight filesystem check before acknowledging a captured baseline.
/// The capture has already hashed bytes twice; this catches writes/renames whose
/// watcher callbacks have not arrived, including preserved modification times.
pub fn snapshot_metadata_matches(
    root: &Path,
    snapshot: &Snapshot,
    control: &crate::resource::Control,
) -> Result<bool> {
    for path in files_controlled(root, control)? {
        if !snapshot.contains_key(&path.to_string_lossy().into_owned()) {
            return Ok(false);
        }
    }
    for (path, stamp) in snapshot {
        control.check()?;
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if stamp.hash == "missing" {
                    continue;
                }
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        if !metadata.is_file()
            || stamp.hash == "missing"
            || metadata.len() != stamp.size
            || modified != stamp.modified
            || changed_time(&metadata) != stamp.changed
        {
            return Ok(false);
        }
    }
    Ok(true)
}
pub fn differences(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|p| before.get(*p).map(|s| &s.hash) != after.get(*p).map(|s| &s.hash))
        .map(PathBuf::from)
        .collect()
}

pub fn extra_inputs(database: &Path, root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = vec![];
    if database.is_file() {
        let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut inputs=conn.prepare("SELECT c.input_paths_json FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=(SELECT current_generation FROM state)")?;
        for row in inputs.query_map([], |r| r.get::<_, String>(0))? {
            result.extend(serde_json::from_str::<Vec<PathBuf>>(&row?)?);
        }
        let mut sources=conn.prepare("SELECT DISTINCT a.path FROM generation_units u JOIN unit_artifacts ua ON ua.owner_generation=u.owner_generation AND ua.compilation_id=u.compilation_id JOIN artifacts a ON a.generation=ua.owner_generation AND a.id=ua.artifact_id WHERE u.generation=(SELECT current_generation FROM state) AND a.generated=0")?;
        for row in sources.query_map([], |r| r.get::<_, String>(0))? {
            result.push(root.join(row?));
        }
    }
    for ancestor in root.ancestors() {
        for name in [
            "global.json",
            "Directory.Build.props",
            "Directory.Build.targets",
            "Directory.Packages.props",
            "NuGet.Config",
            "nuget.config",
            ".npmrc",
            ".swift-version",
            ".tool-versions",
            ".xcode-version",
        ] {
            let path = ancestor.join(name);
            if path.is_file() {
                result.push(path);
            }
        }
    }
    result = result
        .into_iter()
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect();
    result.sort();
    result.dedup();
    Ok(result)
}

// None means a complete refresh; Some scopes replace only those project units.
pub fn affected_projects(
    database: &Path,
    root: &Path,
    changes: &[PathBuf],
) -> Result<Option<Vec<String>>> {
    if !database.is_file() || changes.is_empty() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement=conn.prepare("SELECT DISTINCT c.project_path,c.dependencies_json,c.language,c.input_paths_json FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=(SELECT current_generation FROM state)")?;
    let metadata = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut affected = BTreeSet::new();
    for path in changes {
        let Some(language) = source_language(path) else {
            return Ok(None);
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut statement=conn.prepare_cached("SELECT DISTINCT c.project_path FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id JOIN unit_artifacts ua ON ua.owner_generation=c.generation AND ua.compilation_id=c.id JOIN artifacts a ON a.generation=ua.owner_generation AND a.id=ua.artifact_id WHERE u.generation=(SELECT current_generation FROM state) AND a.path=?")?;
        let mut owners = statement
            .query_map([relative], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (project, _, _, inputs) in &metadata {
            if serde_json::from_str::<Vec<PathBuf>>(inputs)?
                .iter()
                .any(|p| p == path)
            {
                owners.push(project.clone());
            }
        }
        if language == "typescript" {
            affected.insert("$inferred".into());
        }
        if language == "swift" && owners.is_empty() {
            // A newly saved file can introduce a standalone directory group.
            let directory = path.parent().unwrap_or(root);
            let relative = directory.strip_prefix(root).unwrap_or(directory);
            let relative = if relative.as_os_str().is_empty() {
                ".".into()
            } else {
                relative.to_string_lossy().replace('\\', "/")
            };
            affected.insert(format!("$swift-inferred:{relative}"));
        }
        if owners.is_empty() {
            affected.extend(
                metadata
                    .iter()
                    .filter(|(_, _, l, _)| l == language)
                    .map(|(p, _, _, _)| p.clone()),
            );
        } else {
            affected.extend(owners);
        }
    }
    loop {
        let before = affected.len();
        for (project, deps, _, _) in &metadata {
            if serde_json::from_str::<Vec<String>>(deps)?
                .iter()
                .any(|d| affected.contains(d))
            {
                affected.insert(project.clone());
            }
        }
        if affected.len() == before {
            break;
        }
    }
    if affected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(affected.into_iter().collect()))
    }
}

pub struct RefreshOptions<'a> {
    pub configuration: &'a str,
    pub installation: &'a Path,
    pub replacements: Option<&'a [String]>,
    pub preparation_fingerprint: &'a str,
    pub retry_preparation: bool,
    pub expected_identity: &'a str,
    pub settings: &'a crate::protocol::WorkspaceAnalysisSettings,
}

pub fn preparation_fingerprint(inputs: &Snapshot, settings_paths: &[PathBuf]) -> String {
    let mut hash = Sha256::new();
    for (path, stamp) in inputs {
        // Provider-specific effective policies are hashed at the worker boundary.
        // Raw settings bytes must not restore C# merely for a Node trust change.
        if source_language(Path::new(path)).is_none()
            && !settings_paths.iter().any(|p| p == Path::new(path))
        {
            hash.update(path.as_bytes());
            hash.update([0]);
            hash.update(stamp.hash.as_bytes());
            hash.update([0]);
        }
    }
    format!("{hash:x}", hash = hash.finalize())
}

pub fn refresh(
    root: &Path,
    database: &Path,
    options: RefreshOptions<'_>,
    progress: &mut impl FnMut(crate::protocol::Progress),
) -> Result<u64> {
    refresh_controlled(
        root,
        database,
        options,
        progress,
        &crate::resource::Control::default(),
    )
}
pub fn refresh_controlled(
    root: &Path,
    database: &Path,
    options: RefreshOptions<'_>,
    progress: &mut impl FnMut(crate::protocol::Progress),
    control: &crate::resource::Control,
) -> Result<u64> {
    control.check()?;
    let RefreshOptions {
        configuration,
        installation,
        replacements,
        preparation_fingerprint,
        retry_preparation,
        expected_identity,
        settings,
    } = options;
    if identity(root)? != expected_identity {
        return Err(Error::Provider(
            "workspace identity changed; register the replacement explicitly".into(),
        ));
    }
    let paths = files_controlled(root, control)?;
    let mut ts = vec![];
    let mut cs = vec![];
    let mut rust = vec![];
    let mut swift = vec![];
    let mut swift_sources = vec![];
    let mut loose_sources = vec![];
    for path in paths {
        if path.file_name().is_some_and(|n| n == "tsconfig.json") {
            ts.push(path.to_string_lossy().into_owned());
        }
        if path.extension().is_some_and(|x| x == "ts" || x == "tsx") {
            loose_sources.push(path.to_string_lossy().into_owned());
        }
        if path.extension().is_some_and(|x| x == "csproj") {
            cs.push(path.to_string_lossy().into_owned());
        }
        if path.file_name().is_some_and(|n| n == "Cargo.toml") {
            rust.push(path.to_string_lossy().into_owned());
        }
        if swift_manifest(&path)
            || path
                .extension()
                .is_some_and(|e| e == "pbxproj" || e == "xcworkspacedata")
        {
            swift.push(path.to_string_lossy().into_owned());
        } else if path.extension().is_some_and(|e| e == "swift") {
            swift_sources.push(path.to_string_lossy().into_owned());
        }
    }
    let selected =
        |p: &str| replacements.is_none_or(|r| r.iter().any(|s| root.join(s) == Path::new(p)));
    let inferred = !loose_sources.is_empty()
        && replacements.is_none_or(|r| r.iter().any(|s| s == "$inferred"));
    let configured_projects = ts.clone();
    let swift_projects = swift.clone();
    ts.retain(|p| selected(p));
    cs.retain(|p| selected(p));
    rust.retain(|p| selected(p));
    swift.retain(|p| selected(p));
    let swift_inferred = !swift_sources.is_empty()
        && replacements.is_none_or(|r| r.iter().any(|s| s.starts_with("$swift-inferred:")));
    if let Some(replacements) = replacements {
        swift.extend(
            replacements
                .iter()
                .filter(|s| s.starts_with("$swift-inferred:"))
                .cloned(),
        );
    }
    let mut workers = vec![];
    if inferred || !ts.is_empty() {
        workers.push(WorkerCommand {
            program: "node".into(),
            args: vec![
                // Completed TypeScript programs otherwise accumulate until V8's
                // heap heuristics collect them. This flag affects only the worker.
                "--expose-gc".into(),
                installation
                    .join("providers/typescript/worker.mjs")
                    .to_string_lossy()
                    .into_owned(),
            ],
            request: AnalyzeRequest {
                workspace_root: root.to_string_lossy().into_owned(),
                projects: ts,
                configuration: configuration.into(),
                preparation_fingerprint: preparation_fingerprint.into(),
                retry_preparation,
                loose_sources: if inferred { loose_sources } else { vec![] },
                configured_projects,
                analysis_policy: settings.node.clone().map(|node| {
                    crate::protocol::AnalysisPolicy {
                        version: crate::protocol::ANALYSIS_POLICY_VERSION,
                        settings: Some(crate::protocol::analysis_policy::Settings::Node(node)),
                    }
                }),
                ..Default::default()
            },
        });
    }
    if !cs.is_empty() {
        workers.push(WorkerCommand {
            program: "dotnet".into(),
            args: vec![
                installation
                    .join(if installation.join("runtime.json").is_file() {
                        "providers/dotnet/KnowCode.Provider.DotNet.dll"
                    } else if cfg!(debug_assertions) {
                        "providers/dotnet/bin/Debug/net10.0/KnowCode.Provider.DotNet.dll"
                    } else {
                        "providers/dotnet/bin/Release/net10.0/KnowCode.Provider.DotNet.dll"
                    })
                    .to_string_lossy()
                    .into_owned(),
            ],
            request: AnalyzeRequest {
                workspace_root: root.to_string_lossy().into_owned(),
                projects: cs,
                configuration: configuration.into(),
                preparation_fingerprint: preparation_fingerprint.into(),
                retry_preparation,
                analysis_policy: settings.csharp.clone().map(|context| {
                    crate::protocol::AnalysisPolicy {
                        version: crate::protocol::ANALYSIS_POLICY_VERSION,
                        settings: Some(crate::protocol::analysis_policy::Settings::Csharp(context)),
                    }
                }),
                ..Default::default()
            },
        });
    }
    if !rust.is_empty() {
        workers.push(WorkerCommand {
            program: "node".into(),
            args: vec![
                installation
                    .join("providers/rust/worker.mjs")
                    .to_string_lossy()
                    .into_owned(),
            ],
            request: AnalyzeRequest {
                workspace_root: root.to_string_lossy().into_owned(),
                projects: rust,
                configuration: configuration.into(),
                preparation_fingerprint: preparation_fingerprint.into(),
                retry_preparation,
                ..Default::default()
            },
        });
    }
    if swift_inferred || !swift.is_empty() {
        workers.push(WorkerCommand {
            program: "node".into(),
            args: vec![
                installation
                    .join("providers/swift/worker.mjs")
                    .to_string_lossy()
                    .into_owned(),
            ],
            request: AnalyzeRequest {
                workspace_root: root.to_string_lossy().into_owned(),
                projects: swift,
                configured_projects: swift_projects,
                configuration: configuration.into(),
                preparation_fingerprint: preparation_fingerprint.into(),
                retry_preparation,
                loose_sources: if swift_inferred {
                    swift_sources
                } else {
                    vec![]
                },
                ..Default::default()
            },
        });
    }
    if workers.is_empty() && replacements.is_none() {
        return Err(Error::Provider(
            "no supported TypeScript, C#, Rust Cargo, or Swift source/project inputs found".into(),
        ));
    }
    update::index_controlled(
        database,
        workers,
        replacements,
        progress,
        || {
            if identity(root)? != expected_identity {
                return Err(Error::Provider(
                    "workspace identity changed during analysis; publication discarded".into(),
                ));
            }
            Ok(())
        },
        control,
    )
}
