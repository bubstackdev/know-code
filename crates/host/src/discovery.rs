//! Incremental repository enrollment planning; analysis stays in the coordinator.
use know_code_core::{
    protocol::{ParentDiscoveryRequest, ParentDiscoveryStatus},
    workspace,
};
use rusqlite::{Connection, params};
use std::{
    collections::BTreeMap,
    fs::{self, ReadDir},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const VERSION: u32 = 1;
const SLICE_ENTRIES: usize = 256;
const MAX_DEPTH: usize = 64;
const MAX_PENDING: usize = 4096;
const MAX_ITERATORS: usize = 4;
const MAX_DIRECTORY_ENTRIES: usize = 16_384;
const INTERVAL: Duration = Duration::from_secs(30);
const SLICE_TIME: Duration = Duration::from_millis(10);
const MAX_PARENTS: usize = 64;

struct Directory {
    path: PathBuf,
    entries: ReadDir,
    depth: usize,
    seen: usize,
}
struct Parent {
    identity: String,
    status: ParentDiscoveryStatus,
    current: Option<Directory>,
    pending: Vec<(PathBuf, usize)>,
    next: Instant,
}
pub struct Discovery {
    parents: BTreeMap<PathBuf, Parent>,
    position: usize,
}
impl Discovery {
    pub fn open(registry: &Connection) -> Result<Self> {
        registry.execute_batch("CREATE TABLE IF NOT EXISTS discovery_parents(root TEXT PRIMARY KEY,identity TEXT NOT NULL,exclusions TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS discovery_suppressed(root TEXT PRIMARY KEY);")?;
        let mut parents = BTreeMap::new();
        let mut query = registry
            .prepare("SELECT root,identity,exclusions FROM discovery_parents ORDER BY root")?;
        for row in query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (root, identity, exclusions) = row?;
            let exclusions: Vec<String> = serde_json::from_str(&exclusions)?;
            validate_exclusions(&exclusions)?;
            registered_path(&root)?;
            if identity.is_empty() {
                return Err("discovery parent identity is missing".into());
            }
            if parents.len() >= MAX_PARENTS {
                return Err("too many durable discovery parents".into());
            }
            parents.insert(
                PathBuf::from(&root),
                Parent::new(identity, root, exclusions),
            );
        }
        Ok(Self {
            parents,
            position: 0,
        })
    }
    pub fn statuses(&self) -> Vec<ParentDiscoveryStatus> {
        self.parents.values().map(|p| p.status.clone()).collect()
    }
    pub fn action(
        &mut self,
        registry: &Connection,
        request: ParentDiscoveryRequest,
    ) -> Result<Vec<ParentDiscoveryStatus>> {
        if request.action == "list" {
            if !request.root.is_empty() || !request.exclusions.is_empty() {
                return Err("parents list takes no root or exclusions".into());
            }
            return Ok(self.statuses());
        }
        if !matches!(request.action.as_str(), "add" | "scan" | "remove") {
            return Err("unknown parent discovery action".into());
        }
        if request.action != "add" && !request.exclusions.is_empty() {
            return Err("only parents add replaces exclusions".into());
        }
        let requested = registered_path(&request.root)?;
        // A replaced symlink must not redirect management away from a retained
        // registration. Ordinary aliases still resolve when no exact key exists.
        let root = if request.action != "add"
            && let Some((stored, _)) = self.parents.get_key_value(&requested)
        {
            stored.clone()
        } else {
            fs::canonicalize(&requested).unwrap_or(requested)
        };
        match request.action.as_str() {
            "add" => {
                let root = fs::canonicalize(root)?;
                if !root.is_dir() {
                    return Err("discovery parent must be a directory".into());
                }
                if root.to_str().is_none() || root.as_os_str().len() > 4096 {
                    return Err("discovery parent requires a bounded UTF-8 path".into());
                }
                if !self.parents.contains_key(&root) && self.parents.len() >= MAX_PARENTS {
                    return Err("parent discovery limit is 64 registrations".into());
                }
                validate_exclusions(&request.exclusions)?;
                let exclusions: Vec<_> = request
                    .exclusions
                    .into_iter()
                    .map(|v| PathBuf::from(v).to_string_lossy().into_owned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let identity = workspace::identity(&root)?;
                let text = root.to_string_lossy().into_owned();
                registry.execute("INSERT INTO discovery_parents VALUES(?,?,?) ON CONFLICT(root) DO UPDATE SET identity=excluded.identity,exclusions=excluded.exclusions",params![text,identity,serde_json::to_string(&exclusions)?])?;
                self.parents
                    .insert(root, Parent::new(identity, text, exclusions));
            }
            "scan" => {
                let parent = self
                    .parents
                    .get_mut(&root)
                    .ok_or("parent is not registered")?;
                parent.current = None;
                parent.pending.clear();
                parent.next = Instant::now();
                parent.status.phase = "queued".into();
            }
            "remove" => {
                registry.execute(
                    "DELETE FROM discovery_parents WHERE root=?",
                    [root.to_string_lossy().as_ref()],
                )?;
                self.parents.remove(&root);
            }
            _ => unreachable!(),
        }
        Ok(self.statuses())
    }
    /// One round-robin slice and at most one enrollment; callers keep this
    /// ownership lock through enrollment so remove/reconfigure cannot race it.
    pub fn candidate(&mut self) -> Option<(PathBuf, PathBuf)> {
        let count = self.parents.len();
        if count == 0 {
            return None;
        }
        let keys: Vec<_> = self.parents.keys().cloned().collect();
        for _ in 0..count {
            self.position %= count;
            let key = &keys[self.position];
            self.position += 1;
            let total: usize = self.parents.values().map(|p| p.pending.len()).sum();
            let active = self
                .parents
                .values()
                .filter(|p| p.current.is_some())
                .count();
            let parent = self.parents.get_mut(key).expect("parent key");
            if parent.current.is_none() && active >= MAX_ITERATORS {
                continue;
            }
            if parent.status.phase != "scanning" && Instant::now() < parent.next {
                continue;
            }
            let limit = MAX_PENDING
                .saturating_sub(total)
                .saturating_add(parent.pending.len());
            return parent.step(limit).map(|candidate| (key.clone(), candidate));
        }
        None
    }
    pub fn outcome(&mut self, parent: &Path, suppressed: bool, error: Option<&str>) {
        if let Some(parent) = self.parents.get_mut(parent) {
            if let Some(error) = error {
                parent.error(error);
                if parent.status.phase == "idle" {
                    parent.status.phase = "partial".into();
                }
            } else if suppressed {
                parent.status.suppressed += 1;
            } else {
                parent.status.enrolled += 1;
            }
        }
    }
}
fn registered_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.len() > 4096
        || value.as_bytes().contains(&0)
        || !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err("parent path must be absolute without parent traversal".into());
    }
    Ok(path.to_path_buf())
}
fn validate_exclusions(values: &[String]) -> Result<()> {
    if values.len() > 128 || values.iter().map(String::len).sum::<usize>() > 16 * 1024 {
        return Err("parent exclusions exceed their 128-path/16 KiB budget".into());
    }
    for value in values {
        let path = Path::new(value);
        if value.is_empty()
            || path.is_absolute()
            || value.split('/').any(|part| part == "." || part == "..")
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || value.as_bytes().contains(&0)
        {
            return Err(
                "parent exclusions must be nonempty relative path prefixes without . or ..".into(),
            );
        }
    }
    Ok(())
}
impl Parent {
    fn new(identity: String, root: String, exclusions: Vec<String>) -> Self {
        Self {
            identity,
            status: ParentDiscoveryStatus {
                root,
                exclusions,
                phase: "queued".into(),
                ..Default::default()
            },
            current: None,
            pending: Vec::new(),
            next: Instant::now(),
        }
    }
    fn error(&mut self, message: &str) {
        self.status.errors += 1;
        self.status.error = message.chars().take(1024).collect();
    }
    fn excluded(&self, path: &Path) -> bool {
        let root = Path::new(&self.status.root);
        workspace::ignored(root, path)
            || path.strip_prefix(root).is_ok_and(|relative| {
                self.status
                    .exclusions
                    .iter()
                    .any(|v| relative.starts_with(v))
            })
    }
    fn finish(&mut self) {
        self.status.phase = if self.status.errors == 0 {
            "idle"
        } else {
            "partial"
        }
        .into();
        self.next = Instant::now() + INTERVAL;
    }
    /// Checks again before opening. Discovery is ordinary user-owned filesystem
    /// traversal, not a security boundary against hostile same-user mutation.
    fn visit(&mut self, path: PathBuf, depth: usize) -> Option<PathBuf> {
        if path.to_str().is_none() {
            self.error("non-UTF-8 directory path cannot be enrolled");
            return None;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) => {
                self.error(&format!("{}: {error}", path.display()));
                return None;
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return None;
        }
        match fs::symlink_metadata(path.join(".git")) {
            Ok(git) if git.is_file() || git.is_dir() => {
                self.status.repositories_found += 1;
                return Some(path);
            }
            Ok(_) => {
                self.error(&format!("unsupported .git entry at {}", path.display()));
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                self.error(&format!("{}: {error}", path.display()));
                return None;
            }
        }
        if depth >= MAX_DEPTH {
            self.error("parent scan reached its 64-directory depth guard; narrow the parent or add exclusions");
            return None;
        }
        match fs::read_dir(&path) {
            Ok(entries) => {
                self.current = Some(Directory {
                    path,
                    entries,
                    depth,
                    seen: 0,
                })
            }
            Err(error) => self.error(&format!("{}: {error}", path.display())),
        }
        None
    }
    fn step(&mut self, pending_limit: usize) -> Option<PathBuf> {
        let root = PathBuf::from(&self.status.root);
        if !fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            || !workspace::identity(&root).is_ok_and(|id| id == self.identity)
        {
            self.current = None;
            self.pending.clear();
            self.status.available = false;
            self.status.phase = "unavailable".into();
            self.status.error="parent is missing, inaccessible or has a different identity; restore it or explicitly re-register".into();
            self.next = Instant::now() + INTERVAL;
            return None;
        }
        if self.status.phase != "scanning" {
            self.status = ParentDiscoveryStatus {
                root: self.status.root.clone(),
                exclusions: self.status.exclusions.clone(),
                available: true,
                phase: "scanning".into(),
                ..Default::default()
            };
            // The active root is not queued against the shared pending budget.
            if let Some(repository) = self.visit(root, 0) {
                self.finish();
                return Some(repository);
            }
            if self.current.is_none() {
                self.finish();
                return None;
            }
        }
        let start = Instant::now();
        for _ in 0..SLICE_ENTRIES {
            if start.elapsed() >= SLICE_TIME {
                break;
            }
            if self.current.is_none() {
                let Some((path, depth)) = self.pending.pop() else {
                    break;
                };
                if let Some(repository) = self.visit(path, depth) {
                    if self.pending.is_empty() {
                        self.finish();
                    }
                    return Some(repository);
                }
                continue;
            }
            let directory = self.current.as_mut().expect("active directory");
            let path = directory.path.clone();
            let depth = directory.depth;
            let next = directory.entries.next();
            if next.is_some() {
                directory.seen += 1;
            }
            if directory.seen > MAX_DIRECTORY_ENTRIES {
                self.error("parent scan reached the 16384-entry directory guard; narrow the parent or add exclusions");
                self.current = None;
                continue;
            }
            match next {
                None => self.current = None,
                Some(Err(error)) => self.error(&format!("{}: {error}", path.display())),
                Some(Ok(entry)) => {
                    self.status.scanned_entries += 1;
                    let path = entry.path();
                    if self.excluded(&path) {
                        continue;
                    }
                    match entry.file_type() {
                        Ok(kind) if kind.is_dir() && !kind.is_symlink() => {
                            if self.pending.len() >= pending_limit {
                                self.error("parent scan reached the shared 4096 pending-directory guard; narrow parents or add exclusions");
                            } else {
                                self.pending.push((path, depth + 1));
                            }
                        }
                        Ok(_) => {}
                        Err(error) => self.error(&format!("{}: {error}", path.display())),
                    }
                }
            }
        }
        if self.current.is_none() && self.pending.is_empty() {
            self.finish();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    fn root() -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/discovery-unit")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }
    fn add(d: &mut Discovery, c: &Connection, path: &Path, exclusions: Vec<String>) {
        d.action(
            c,
            ParentDiscoveryRequest {
                action: "add".into(),
                root: path.to_str().unwrap().into(),
                exclusions,
            },
        )
        .unwrap();
    }
    fn scan(d: &mut Discovery) -> Vec<PathBuf> {
        let mut found = vec![];
        for _ in 0..10000 {
            let before: u64 = d.statuses().iter().map(|s| s.scanned_entries).sum();
            if let Some((_, path)) = d.candidate() {
                found.push(path);
            }
            assert!(d.parents.values().filter(|p| p.current.is_some()).count() <= MAX_ITERATORS);
            assert!(d.parents.values().map(|p| p.pending.len()).sum::<usize>() <= MAX_PENDING);
            let after: u64 = d.statuses().iter().map(|s| s.scanned_entries).sum();
            assert!(after.saturating_sub(before) <= SLICE_ENTRIES as u64);
            if d.statuses()
                .iter()
                .all(|s| s.phase != "scanning" && s.phase != "queued")
            {
                return found;
            }
        }
        panic!("scan did not finish")
    }
    #[test]
    fn boundaries_exclusions_symlinks_and_resume_are_explicit() {
        let root = root();
        let c = Connection::open_in_memory().unwrap();
        let mut d = Discovery::open(&c).unwrap();
        for path in [
            "repo/.git",
            "repo/nested/.git",
            "excluded/r/.git",
            "other/.git",
            "node_modules/hidden/.git",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::create_dir(root.join("linked")).unwrap();
        fs::write(
            root.join("linked/.git"),
            "gitdir: separately-owned-worktree",
        )
        .unwrap();
        symlink(root.join("other"), root.join("alias")).unwrap();
        for i in 0..600 {
            fs::write(root.join(format!("file-{i}")), "").unwrap();
        }
        add(&mut d, &c, &root, vec!["excluded".into()]);
        let mut found = scan(&mut d);
        found.sort();
        assert_eq!(
            found,
            vec![root.join("linked"), root.join("other"), root.join("repo")]
        );
        assert!(d.statuses()[0].scanned_entries > 600);
        assert_eq!(d.statuses()[0].phase, "idle", "{:?}", d.statuses());
        assert!(d.candidate().is_none());
    }
    #[test]
    fn registrations_exclusions_and_identity_survive_restart() {
        let root = root();
        let c = Connection::open_in_memory().unwrap();
        let mut d = Discovery::open(&c).unwrap();
        for value in ["", "..", "/outside", "a/../b", ".", "a/./b"] {
            assert!(
                d.action(
                    &c,
                    ParentDiscoveryRequest {
                        action: "add".into(),
                        root: root.to_str().unwrap().into(),
                        exclusions: vec![value.into()]
                    }
                )
                .is_err()
            );
        }
        add(&mut d, &c, &root, vec!["hold".into()]);
        drop(d);
        let mut d = Discovery::open(&c).unwrap();
        assert_eq!(d.statuses()[0].exclusions, vec!["hold"]);
        let held = root.with_extension("held");
        fs::rename(&root, &held).unwrap();
        fs::create_dir(&root).unwrap();
        scan(&mut d);
        assert_eq!(d.statuses()[0].phase, "unavailable");
        add(&mut d, &c, &root, vec![]);
        scan(&mut d);
        assert_eq!(d.statuses()[0].phase, "idle");
        d.action(
            &c,
            ParentDiscoveryRequest {
                action: "remove".into(),
                root: root.to_str().unwrap().into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(Discovery::open(&c).unwrap().statuses().is_empty());
    }
    #[test]
    fn guard_and_round_robin_report_partial_instead_of_silent_completion() {
        let root = root();
        let c = Connection::open_in_memory().unwrap();
        let mut d = Discovery::open(&c).unwrap();
        let mut deep = root.join("deep");
        fs::create_dir(&deep).unwrap();
        let parent = deep.clone();
        for _ in 0..MAX_DEPTH + 2 {
            deep = deep.join("d");
            fs::create_dir(&deep).unwrap();
        }
        fs::create_dir(deep.join(".git")).unwrap();
        let other = root.join("other");
        fs::create_dir_all(other.join(".git")).unwrap();
        add(&mut d, &c, &parent, vec![]);
        add(&mut d, &c, &other, vec![]);
        assert_eq!(scan(&mut d), vec![other]);
        let partial = d
            .statuses()
            .into_iter()
            .find(|s| s.root == parent.to_str().unwrap())
            .unwrap();
        assert_eq!(partial.phase, "partial");
        assert_eq!(partial.errors, 1);
        assert!(partial.error.contains("depth guard"));
    }
    #[test]
    fn iterator_pool_and_pending_entry_guards_stay_shared_and_explicit() {
        let root = root();
        let c = Connection::open_in_memory().unwrap();
        let mut d = Discovery::open(&c).unwrap();
        for i in 0..MAX_ITERATORS + 2 {
            let path = root.join(format!("parent-{i}"));
            fs::create_dir(&path).unwrap();
            for j in 0..SLICE_ENTRIES + 10 {
                fs::write(path.join(format!("file-{j}")), "").unwrap();
            }
            add(&mut d, &c, &path, vec![]);
        }
        assert!(scan(&mut d).is_empty());
        assert!(d.statuses().iter().all(|s| s.phase == "idle"));
        let path = root.join("bounded");
        fs::create_dir_all(path.join("child/.git")).unwrap();
        let mut parent = Parent::new(
            workspace::identity(&path).unwrap(),
            path.to_str().unwrap().into(),
            vec![],
        );
        assert!(parent.step(0).is_none());
        assert!(parent.pending.is_empty());
        assert!(parent.status.errors > 0);
        assert!(parent.status.error.contains("pending-directory guard"));
        let path = root.join("entries");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("a"), "").unwrap();
        fs::write(path.join("b"), "").unwrap();
        let mut parent = Parent::new(
            workspace::identity(&path).unwrap(),
            path.to_str().unwrap().into(),
            vec![],
        );
        parent.status.phase = "scanning".into();
        parent.visit(path, 0);
        parent.current.as_mut().unwrap().seen = MAX_DIRECTORY_ENTRIES;
        parent.step(MAX_PENDING);
        assert_eq!(parent.status.phase, "partial");
        assert!(parent.status.error.contains("entry directory guard"));
        assert!(parent.current.is_none());
    }
    #[test]
    fn management_uses_retained_key_before_replaced_symlink_target() {
        let root = root();
        let path = root.join("parent");
        let other = root.join("other");
        fs::create_dir(&path).unwrap();
        fs::create_dir(&other).unwrap();
        let c = Connection::open_in_memory().unwrap();
        let mut d = Discovery::open(&c).unwrap();
        add(&mut d, &c, &path, vec![]);
        let held = root.join("held");
        fs::rename(&path, &held).unwrap();
        symlink(&other, &path).unwrap();
        d.action(
            &c,
            ParentDiscoveryRequest {
                action: "scan".into(),
                root: path.to_str().unwrap().into(),
                ..Default::default()
            },
        )
        .unwrap();
        scan(&mut d);
        assert_eq!(d.statuses()[0].phase, "unavailable");
        d.action(
            &c,
            ParentDiscoveryRequest {
                action: "remove".into(),
                root: path.to_str().unwrap().into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(d.statuses().is_empty());
        assert!(Discovery::open(&c).unwrap().statuses().is_empty());
        assert!(other.exists() && held.exists());
        fs::remove_file(&path).unwrap();
        fs::rename(&held, &path).unwrap();
        add(&mut d, &c, &path, vec![]);
        let alias = root.join("alias");
        symlink(&path, &alias).unwrap();
        d.action(
            &c,
            ParentDiscoveryRequest {
                action: "remove".into(),
                root: alias.to_str().unwrap().into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(d.statuses().is_empty());
    }
}
