//! SDK packages a repo declares as an external client, read from its checkout's `node_modules`: which request each of
//! the client's methods makes, so a call to it links through the package's code instead of by the method's name.
//!
//! This is the one place KnowCode reads files that aren't the repo's own (see [`crate::discover`]: dependency
//! directories are never indexed), and it is kept narrow:
//!
//! - Only a package the repo declares as an external client: a `.d.ts` file of the repo writes `declare module
//!   '<package>'` (the repo's own typing of an untyped client, such as `jellyfin-apiclient`).
//! - Only the copy installed at `node_modules/<package>` nearest the declaring file, and only when its
//!   `package.json` version is the one the nearest lockfile (`package-lock.json`, `npm-shrinkwrap.json`,
//!   `yarn.lock`) resolves: what the lockfile pins is what runs. A package missing, unpinned, installed at another
//!   version, or a symbolic link (never followed) isn't read, and why is kept ([`SdkPackage::unread`]).
//! - The repo's AI privacy file applies to `node_modules` as to any path of the repo: a blocked file isn't read.
//! - Readable JavaScript is read as it is; a minified file is read from its source map's `sourcesContent` (the
//!   original sources it was built from), and cited as the map and the original path and line.
//! - What is read is extracted in memory and kept in the repo's index, in the cache: nothing is written into the
//!   repo, and nothing is fetched.
//! - `index.node_modules = false` in `config.toml` turns it off.
//!
//! For each method of each class in the package's code, the requests it makes ([`SdkMethod::requests`]): its own,
//! else those of the class's methods it calls (`this.getJSON(url)` in `getItems`), one call deep.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use kc_extract::Lang;
use kc_store::{FileRecord, Store};
use serde::{Deserialize, Serialize};

use crate::privacy::Privacy;

/// The `meta` key holding the packages read ([`SdkPackage`] as JSON), and the one holding what they were read from.
pub const META_KEY: &str = "sdk.packages";
const META_STAMP: &str = "sdk.stamp";

/// Moves when what is read from a package, or how, changes: every package is read again.
const SDK_VERSION: u32 = 1;

/// At most this many files, and bytes, are read from one package.
const MAX_FILES: usize = 400;
const MAX_BYTES: u64 = 16 << 20;

/// A package a repo declares as an external client, and what was read from it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SdkPackage {
    /// The package's name: `jellyfin-apiclient`.
    pub name: String,
    /// Where the repo declares it: `src/apiclient.d.ts`.
    pub declared_in: String,
    /// The installed version, when one is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// The installed copy, repo-relative: `node_modules/jellyfin-apiclient`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
    /// Why the package's code wasn't read, when it wasn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<String>,
    /// Every method of a class in its code, with the requests it makes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<SdkMethod>,
}

/// A method of a class in a package's code.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SdkMethod {
    pub class: String,
    pub method: String,
    /// Where it is: the file read, or the original source a source map holds (`src/apiClient.js`), and its line.
    pub file: String,
    pub line: u32,
    /// The source map it was read from, repo-relative, when it was read from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    /// The requests it makes, its own or those of the class's methods it calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<SdkRequest>,
}

impl SdkMethod {
    /// Where the method is, as an answer cites it: `src/apiClient.js:812 (node_modules/…/jellyfin-apiclient.js.map)`.
    pub fn at(&self) -> String {
        match &self.map {
            Some(m) => format!("{}:{} (from {m})", self.file, self.line),
            None => format!("{}:{}", self.file, self.line),
        }
    }
}

/// A request a package method makes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SdkRequest {
    pub verb: String,
    /// The path, or `None` when its URL is known only at run time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The line making it.
    pub line: u32,
    /// The class's method making it, when the method makes it through another (`ApiClient.getJSON`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// Whether reading declared packages from `node_modules` is on (`index.node_modules`, on by default).
fn enabled() -> bool {
    crate::config::node_modules().map(|(on, _)| on).unwrap_or(true)
}

/// Reads the packages the repo at `root` declares as external clients again when what they are read from changed
/// (the declaring files, the installed `package.json`s, the lockfiles, the setting), and stores them in the index.
/// Returns whether the stored packages changed. Best effort: a package that can't be read says why.
pub fn refresh(root: &Path, store: &mut Store, privacy: &Privacy) -> Result<bool> {
    let on = enabled();
    let dts: Vec<(String, String)> = store
        .conn()
        .prepare("SELECT path, hash FROM files WHERE path LIKE '%.d.ts' ORDER BY path")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let declared = if on { declared(root, &dts, privacy) } else { Vec::new() };
    let stamp = stamp_of(root, on, &dts, &declared);
    if store.meta(META_STAMP)?.as_deref() == Some(stamp.as_str()) {
        return Ok(false);
    }
    let packages: Vec<SdkPackage> = declared.iter().map(|(name, file)| read_package(root, name, file, privacy)).collect();
    let json = serde_json::to_string(&packages)?;
    let changed = store.meta(META_KEY)?.unwrap_or_else(|| "[]".to_string()) != json;
    let batch = store.batch()?;
    batch.set_meta(META_KEY, &json)?;
    batch.set_meta(META_STAMP, &stamp)?;
    if changed {
        batch.set_meta(crate::SUMMARY_GEN, &format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())))?;
    }
    batch.commit()?;
    Ok(changed)
}

/// The packages stored for the repo `conn` indexes.
pub fn stored(conn: &rusqlite::Connection) -> Result<Vec<SdkPackage>> {
    let json: Option<String> =
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [META_KEY], |r| r.get(0)).ok();
    Ok(json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
}

/// The packages the repo's `.d.ts` files declare (`declare module 'name'`), each with the first file declaring it.
fn declared(root: &Path, dts: &[(String, String)], privacy: &Privacy) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (path, _) in dts {
        if privacy.blocks(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(path)) else { continue };
        for name in declared_modules(&text) {
            if !out.iter().any(|(n, _)| *n == name) {
                out.push((name, path.clone()));
            }
        }
    }
    out
}

/// The package names a `.d.ts` declares as modules: `declare module 'jellyfin-apiclient' {`. Relative and wildcard
/// module names (`'./x'`, `'*.svg'`) are no packages.
pub fn declared_modules(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("declare module") else { continue };
        let rest = rest.trim_start();
        let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) else { continue };
        let Some(name) = rest[1..].split(q).next() else { continue };
        if is_package_name(name) && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// An npm package name: `name` or `@scope/name`, lower-case letters, digits, `-`, `_`, `.`.
fn is_package_name(name: &str) -> bool {
    let ok = |s: &str| !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    match name.strip_prefix('@') {
        Some(scoped) => scoped.split_once('/').is_some_and(|(s, n)| ok(s) && ok(n)),
        None => ok(name),
    }
}

/// What the stored packages were read from: the setting, the declaring files, and each package's installed
/// `package.json` and lockfile, by stat.
fn stamp_of(root: &Path, on: bool, dts: &[(String, String)], declared: &[(String, String)]) -> String {
    let mut parts = vec![format!("v{SDK_VERSION}.{}", Lang::JavaScript.extractor_version()), on.to_string()];
    for (p, h) in dts {
        parts.push(format!("{p}={h}"));
    }
    for (name, file) in declared {
        let (dir, lock) = locate(root, name, file);
        parts.push(format!("{name}:{}", dir.as_ref().map(|d| crate::git::stamp(&root.join(d).join("package.json"))).unwrap_or_default()));
        if let Some(l) = lock {
            parts.push(format!("{l}:{}", crate::git::stamp(&root.join(&l))));
        }
    }
    blake3::hash(parts.join("\n").as_bytes()).to_hex().to_string()
}

/// The repo-relative directory a package is installed at for code in `file` (the nearest `node_modules/<name>`
/// from its directory up to the root, as Node resolves it), and the nearest lockfile at or above the directory
/// holding that `node_modules`.
fn locate(root: &Path, name: &str, file: &str) -> (Option<String>, Option<String>) {
    let mut dir = Path::new(file).parent().map(Path::to_path_buf).unwrap_or_default();
    loop {
        let candidate = dir.join("node_modules").join(name);
        if std::fs::symlink_metadata(root.join(&candidate)).is_ok() {
            let lock = lockfile(root, &dir);
            return (Some(rel(&candidate)), lock);
        }
        if !dir.pop() {
            return (None, None);
        }
    }
}

/// The nearest lockfile at or above `dir` (repo-relative), up to the root.
fn lockfile(root: &Path, dir: &Path) -> Option<String> {
    let mut dir = dir.to_path_buf();
    loop {
        for f in ["package-lock.json", "npm-shrinkwrap.json", "yarn.lock"] {
            let p = dir.join(f);
            if root.join(&p).is_file() {
                return Some(rel(&p));
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn rel(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").trim_start_matches("./").to_string()
}

/// Whether any component of the repo-relative `path` is a symbolic link.
fn through_link(root: &Path, path: &str) -> bool {
    let mut at = root.to_path_buf();
    path.split('/').any(|c| {
        at.push(c);
        std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink())
    })
}

/// Reads the package `name` the repo declares in `file`: its version against the lockfile's, then its code.
fn read_package(root: &Path, name: &str, file: &str, privacy: &Privacy) -> SdkPackage {
    let mut p = SdkPackage { name: name.to_string(), declared_in: file.to_string(), ..SdkPackage::default() };
    let (dir, lock) = locate(root, name, file);
    let Some(dir) = dir else {
        p.unread = Some(format!("not installed (no node_modules/{name} at or above {file})"));
        return p;
    };
    p.dir = dir.clone();
    if through_link(root, &dir) {
        p.unread = Some(format!("{dir} is a symbolic link, which KnowCode doesn't follow"));
        return p;
    }
    let manifest = format!("{dir}/package.json");
    if privacy.blocks(&manifest) {
        p.unread = Some(format!("{dir} is blocked by the repo's privacy file"));
        return p;
    }
    let pkg: serde_json::Value = match std::fs::read(root.join(&manifest)).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(v) => v,
        None => {
            p.unread = Some(format!("{manifest} can't be read"));
            return p;
        }
    };
    p.version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let Some(lock) = lock else {
        p.unread = Some("no lockfile pins its version".to_string());
        return p;
    };
    let pinned = std::fs::read_to_string(root.join(&lock)).ok().and_then(|t| locked_version(&lock, &t, name, &dir));
    match pinned {
        None => {
            p.unread = Some(format!("{lock} doesn't pin it"));
            return p;
        }
        Some(v) if !same_version(&v, &p.version) => {
            p.unread = Some(format!("installed {}, but {lock} pins {v}", p.version));
            return p;
        }
        Some(_) => {}
    }
    let (sources, skipped) = sources(root, &dir, privacy);
    if sources.is_empty() {
        p.unread = Some(skipped.unwrap_or_else(|| "no JavaScript or TypeScript KnowCode reads".to_string()));
        return p;
    }
    match methods(&sources) {
        Ok(m) => p.methods = m,
        Err(e) => p.unread = Some(format!("its code couldn't be read: {e}")),
    }
    p
}

/// The version `lock` (its repo-relative path; `text` its content) resolves for the package installed at `dir`.
pub fn locked_version(lock: &str, text: &str, name: &str, dir: &str) -> Option<String> {
    if lock.ends_with("yarn.lock") {
        return yarn_version(text, name);
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    // npm 7+: `packages` keyed by the install path relative to the lockfile's directory.
    if let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) {
        let base = Path::new(lock).parent().map(rel).unwrap_or_default();
        let key = dir.strip_prefix(&base).map(|k| k.trim_start_matches('/')).unwrap_or(dir);
        if let Some(found) = pkgs.get(key).and_then(|e| e.get("version")).and_then(|x| x.as_str()) {
            return Some(found.to_string());
        }
    }
    // npm 6: `dependencies` keyed by name.
    v.get("dependencies").and_then(|d| d.get(name)).and_then(|e| e.get("version")).and_then(|x| x.as_str()).map(str::to_string)
}

/// The version a `yarn.lock` (classic or Berry) resolves for `name`, when its entries for it agree on one.
fn yarn_version(text: &str, name: &str) -> Option<String> {
    let mut found: Option<String> = None;
    let mut ours = false;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') && line.trim_end().ends_with(':') {
            // `name@^1.0.0, name@~1.0.0:`, `"@scope/name@npm:^1.0.0":`.
            ours = line.trim_end().trim_end_matches(':').split(',').any(|spec| {
                let spec = spec.trim().trim_matches('"');
                spec.rsplit_once('@').is_some_and(|(n, _)| n == name)
            });
            continue;
        }
        if ours {
            let t = line.trim();
            if let Some(v) = t.strip_prefix("version ").or_else(|| t.strip_prefix("version: ")) {
                let v = v.trim().trim_matches('"').to_string();
                if found.as_ref().is_some_and(|f| *f != v) {
                    return None;
                }
                found = Some(v);
            }
        }
    }
    found
}

/// Two versions are the same when they are equal but for build metadata (`+commit.…`), which semver ignores.
fn same_version(a: &str, b: &str) -> bool {
    let core = |v: &str| v.split('+').next().unwrap_or(v).trim().to_string();
    !a.is_empty() && core(a) == core(b)
}

/// A source to extract: its path as cited, its text, and the map it came from.
struct Source {
    path: String,
    text: String,
    map: Option<String>,
}

/// The package's JavaScript and TypeScript under `dir` (nested `node_modules` left out), a minified file through its
/// source map's original sources, and why a file was left out when nothing could be read.
fn sources(root: &Path, dir: &str, privacy: &Privacy) -> (Vec<Source>, Option<String>) {
    let mut files: Vec<String> = Vec::new();
    let mut stack = vec![root.join(dir)];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(t) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if t.is_symlink() || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if t.is_dir() {
                stack.push(e.path());
            } else if is_code(&name)
                && !name.ends_with(".d.ts")
                && let Ok(r) = e.path().strip_prefix(root)
            {
                files.push(rel(r));
            }
        }
    }
    files.sort();
    let mut out: Vec<Source> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut bytes = 0u64;
    let mut why = None;
    for f in files {
        if out.len() >= MAX_FILES || bytes >= MAX_BYTES {
            break;
        }
        if privacy.blocks(&f) {
            continue;
        }
        let Ok(raw) = std::fs::read(root.join(&f)) else { continue };
        let text = kc_extract::decode(&raw).into_owned();
        if !kc_extract::is_minified(&text) {
            if raw.len() as u64 <= crate::MAX_FILE_BYTES {
                bytes += raw.len() as u64;
                let path = f.strip_prefix(dir).map(|p| p.trim_start_matches('/').to_string()).unwrap_or(f.clone());
                if seen.insert(path.clone()) {
                    out.push(Source { path, text, map: None });
                }
            }
            continue;
        }
        // Minified: its original sources, from its source map.
        let Some(map) = map_of(root, &f, &text).filter(|m| !privacy.blocks(m)) else {
            why = Some(format!("{f} is minified, with no source map holding its sources"));
            continue;
        };
        let Some(original) = std::fs::read(root.join(&map)).ok().and_then(|b| map_sources(&b)) else {
            why = Some(format!("{map} holds no sources"));
            continue;
        };
        for (path, text) in original {
            if out.len() >= MAX_FILES || bytes >= MAX_BYTES {
                break;
            }
            if seen.insert(path.clone()) {
                bytes += text.len() as u64;
                out.push(Source { path, text, map: Some(map.clone()) });
            }
        }
    }
    (out, why)
}

fn is_code(name: &str) -> bool {
    [".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx"].iter().any(|e| name.ends_with(e))
}

/// The source map of the minified file `f` (repo-relative): the file its last `//# sourceMappingURL=` names, else
/// `<f>.map`, when it exists.
fn map_of(root: &Path, f: &str, text: &str) -> Option<String> {
    let named = text
        .lines()
        .rev()
        .take(3)
        .find_map(|l| l.trim().strip_prefix("//# sourceMappingURL=").map(str::trim))
        .filter(|u| !u.contains(':') && !u.starts_with('/'));
    let dir = Path::new(f).parent().map(Path::to_path_buf).unwrap_or_default();
    let candidates: Vec<PathBuf> = match named {
        Some(u) => vec![dir.join(u)],
        None => vec![PathBuf::from(format!("{f}.map"))],
    };
    candidates.into_iter().map(|p| rel(&p)).find(|p| !p.split('/').any(|c| c == "..") && root.join(p).is_file())
}

/// A source map's original sources with their text: `sources` paired with `sourcesContent`, bundler prefixes
/// (`webpack://name/./src/a.js`) dropped, the bundler's own runtime and other packages' sources (under
/// `node_modules`) left out.
pub fn map_sources(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let sources = v.get("sources")?.as_array()?;
    let contents = v.get("sourcesContent")?.as_array()?;
    let mut out = Vec::new();
    for (s, c) in sources.iter().zip(contents) {
        let (Some(s), Some(c)) = (s.as_str(), c.as_str()) else { continue };
        let path = match s.split_once("://") {
            // `webpack://jellyfin-apiclient/./src/apiClient.js`: the package's name, then the path.
            Some((_, rest)) => rest.split_once('/').map_or(rest, |(_, p)| p),
            None => s,
        };
        let path = path.trim_start_matches("./").to_string();
        if path.is_empty() || path.contains("node_modules/") || path.starts_with("webpack/") || path.starts_with('(') || path.split('/').any(|c| c == "..") || !is_code(&path) {
            continue;
        }
        out.push((path, c.to_string()));
    }
    (!out.is_empty()).then_some(out)
}

/// A method as [`methods`] reads it: (symbol id, name, class, first line, file).
type MethodRow = (i64, String, String, u32, String);

/// Every method of a class in the sources, with the requests it makes: extracted into a throwaway in-memory index,
/// so the routes come out as the repo's own do (constants resolved, URL builders read).
fn methods(sources: &[Source]) -> Result<Vec<SdkMethod>> {
    let mut store = Store::open(Path::new(":memory:"))?;
    {
        let batch = store.batch()?;
        for s in sources {
            let tsx = !s.path.ends_with(".ts");
            let lang = if s.path.ends_with(".ts") { Lang::TypeScript } else { Lang::JavaScript };
            let facts = kc_extract::extract_package_source(&s.text, tsx);
            let hash = blake3::hash(s.text.as_bytes()).to_hex().to_string();
            batch.put(&FileRecord { path: &s.path, lang, mtime_ns: 0, size: s.text.len() as i64, hash: &hash, lines: s.text.lines().count() as u32, facts: &facts })?;
        }
        batch.commit()?;
    }
    let conn = store.conn();
    let maps: HashMap<&str, &str> = sources.iter().filter_map(|s| s.map.as_deref().map(|m| (s.path.as_str(), m))).collect();
    // Requests by the method making them: (class.method, start line).
    let mut own: HashMap<(String, u32), Vec<SdkRequest>> = HashMap::new();
    for r in kc_store::query::http_routes(conn)? {
        if !matches!(r.side.as_str(), "client" | "unresolved") {
            continue;
        }
        let path = if r.side == "client" { r.path.clone() } else { None };
        own.entry((r.symbol.clone(), r.start_line)).or_default().push(SdkRequest { verb: r.verb.clone(), path, line: r.line, via: None });
    }
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.container, s.start_line, f.path FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.kind = 'method' AND s.container IS NOT NULL ORDER BY f.path, s.start_line",
    )?;
    let rows: Vec<MethodRow> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    // The class's methods each method calls on `this`.
    let mut calls = conn.prepare("SELECT DISTINCT name FROM refs WHERE from_id = ?1 AND kind = 'call' AND receiver = 'this' ORDER BY line")?;
    let by_name: HashMap<(&str, &str), &MethodRow> = rows.iter().map(|r| ((r.2.as_str(), r.1.as_str()), r)).collect();
    let mut out = Vec::new();
    for (id, name, class, line, file) in &rows {
        let class_short = class.rsplit('.').next().unwrap_or(class).to_string();
        let mut requests = own.get(&(format!("{class}.{name}"), *line)).cloned().unwrap_or_default();
        if requests.is_empty() {
            let called: Vec<String> = calls.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for c in called {
                let Some(target) = by_name.get(&(class.as_str(), c.as_str())) else { continue };
                for mut q in own.get(&(format!("{}.{}", target.2, target.1), target.3)).cloned().unwrap_or_default() {
                    q.via = Some(format!("{class_short}.{c}"));
                    requests.push(q);
                }
            }
        }
        out.push(SdkMethod {
            class: class_short,
            method: name.clone(),
            file: file.clone(),
            line: *line,
            map: maps.get(file.as_str()).map(|m| m.to_string()),
            requests,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_modules_are_package_names() {
        let text = "declare module 'media-apiclient' {\n  class ApiClient {}\n}\ndeclare module \"@media/sdk\" {}\ndeclare module '*.svg';\ndeclare module './local';\n";
        assert_eq!(declared_modules(text), ["media-apiclient", "@media/sdk"]);
    }

    #[test]
    fn lockfiles_pin_versions() {
        let npm7 = r#"{"packages": {"": {}, "node_modules/media-apiclient": {"version": "1.2.0"}, "web/node_modules/@media/sdk": {"version": "0.3.1"}}}"#;
        assert_eq!(locked_version("package-lock.json", npm7, "media-apiclient", "node_modules/media-apiclient").as_deref(), Some("1.2.0"));
        assert_eq!(locked_version("package-lock.json", npm7, "@media/sdk", "web/node_modules/@media/sdk").as_deref(), Some("0.3.1"));
        // A lockfile in a subdirectory keys its own install paths.
        assert_eq!(locked_version("web/package-lock.json", r#"{"packages": {"node_modules/x": {"version": "2.0.0"}}}"#, "x", "web/node_modules/x").as_deref(), Some("2.0.0"));
        let npm6 = r#"{"dependencies": {"media-apiclient": {"version": "1.1.0"}}}"#;
        assert_eq!(locked_version("package-lock.json", npm6, "media-apiclient", "node_modules/media-apiclient").as_deref(), Some("1.1.0"));
        let yarn = "# yarn lockfile v1\n\nmedia-apiclient@^1.2.0, media-apiclient@~1.2.0:\n  version \"1.2.0\"\n  resolved \"x\"\n\n\"@media/sdk@npm:^0.3.0\":\n  version: 0.3.1\n";
        assert_eq!(locked_version("yarn.lock", yarn, "media-apiclient", "").as_deref(), Some("1.2.0"));
        assert_eq!(locked_version("yarn.lock", yarn, "@media/sdk", "").as_deref(), Some("0.3.1"));
        assert_eq!(locked_version("yarn.lock", yarn, "other", ""), None);
        // Build metadata doesn't count.
        assert!(same_version("0.0.0-unstable.1", "0.0.0-unstable.1+commit.abc"));
        assert!(!same_version("1.2.0", "1.2.1"));
    }

    /// The repo's privacy file applies to `node_modules`; a package installed through a symbolic link isn't read.
    #[test]
    fn privacy_and_links_keep_a_package_unread() {
        let root = std::env::temp_dir().join(format!("knowcode-sdk-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pkg = root.join("node_modules/media-apiclient");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{ "version": "1.2.0" }"#).unwrap();
        std::fs::write(pkg.join("index.js"), "export class ApiClient {\n  ping() { return fetch('/System/Ping', { method: 'POST' }); }\n}\n").unwrap();
        std::fs::write(root.join("package-lock.json"), r#"{ "packages": { "node_modules/media-apiclient": { "version": "1.2.0" } } }"#).unwrap();
        let format = crate::privacy::Format::default();
        let open = Privacy::from_text(None, &format, 1, crate::privacy::Malformed::Restricted);
        let read = read_package(&root, "media-apiclient", "src/apiclient.d.ts", &open);
        assert_eq!(read.unread, None);
        assert_eq!(read.methods.iter().map(|m| (m.method.as_str(), m.requests.len())).collect::<Vec<_>>(), [("ping", 1)]);
        let blocked = Privacy::from_text(Some(Ok("rules:\n  - path: node_modules/\n    level: 1\n".into())), &format, 1, crate::privacy::Malformed::Restricted);
        let p = read_package(&root, "media-apiclient", "src/apiclient.d.ts", &blocked);
        assert!(p.methods.is_empty() && p.unread.as_deref().is_some_and(|u| u.contains("blocked by the repo's privacy file")), "{p:?}");
        // Installed through a link (a workspace's or pnpm's): not followed.
        let real = root.join("store/media-apiclient");
        std::fs::create_dir_all(root.join("store")).unwrap();
        std::fs::rename(&pkg, &real).unwrap();
        std::os::unix::fs::symlink(&real, &pkg).unwrap();
        let p = read_package(&root, "media-apiclient", "src/apiclient.d.ts", &open);
        assert!(p.methods.is_empty() && p.unread.as_deref().is_some_and(|u| u.contains("symbolic link")), "{p:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_maps_give_original_sources() {
        let map = r#"{"version": 3, "sources": ["webpack://media-apiclient/webpack/universalModuleDefinition", "webpack://media-apiclient/./src/apiClient.js", "webpack://media-apiclient/./node_modules/dep/index.js", "../src/events.js"],
                      "sourcesContent": ["(function(){})", "export class ApiClient {}", "x", "export const e = 1;"]}"#;
        let got = map_sources(map.as_bytes()).unwrap();
        assert_eq!(got.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["src/apiClient.js"]);
    }

    /// A class's methods with the requests each makes: its own, else those of the methods it calls on `this`, one
    /// call deep; a method making none has none.
    #[test]
    fn methods_have_their_requests() {
        let src = "export default class ApiClient {\n  getUrl(name) { return this._base + '/' + name; }\n  getJSON(url) { return this.ajax({ type: 'GET', url: url, dataType: 'json' }); }\n  getItems(userId) {\n    const url = this.getUrl(`Users/${userId}/Items`);\n    return this.getJSON(url);\n  }\n  markPlayed(userId, itemId) {\n    return this.ajax({ type: 'POST', url: this.getUrl(`Users/${userId}/PlayedItems/${itemId}`) });\n  }\n  getCurrentUser() { return this.getItems(this._userId); }\n  serverId() { return this._serverInfo.Id; }\n}\n";
        let m = methods(&[Source { path: "src/apiClient.js".into(), text: src.into(), map: Some("dist/a.js.map".into()) }]).unwrap();
        let of = |n: &str| m.iter().find(|x| x.method == n).unwrap();
        let reqs = |n: &str| of(n).requests.iter().map(|r| (r.verb.clone(), r.path.clone(), r.via.clone())).collect::<Vec<_>>();
        assert_eq!(reqs("getItems"), [("GET".to_string(), Some("Users/{}/Items".to_string()), None)]);
        assert_eq!(reqs("markPlayed"), [("POST".to_string(), Some("Users/{}/PlayedItems/{}".to_string()), None)]);
        // One call deep.
        assert_eq!(reqs("getCurrentUser"), [("GET".to_string(), Some("Users/{}/Items".to_string()), Some("ApiClient.getItems".to_string()))]);
        assert!(reqs("serverId").is_empty());
        assert_eq!(of("getItems").at(), "src/apiClient.js:4 (from dist/a.js.map)");
    }
}
