//! Notes anchored to symbols, after codebase-memory-mcp's decision records and Sverklo's commit-pinned memory: a
//! short text an agent or a person attaches to a symbol, which says when the code under it has changed.
//!
//! Notes live in `notes.sqlite` in the user data directory (`~/Library/Application Support/dev.bubstack.knowcode` on
//! macOS; `notes.path` in `config.toml` or `KNOWCODE_DATA_DIR` move it), never in the cache: they are written by people
//! and agents, not derived from the code, and may be the only record of a decision, so clearing the cache (`purge`,
//! the OS, a new machine) never loses them, and reindexing, `forget` and `prune` never touch them. Nothing is written
//! into the repo. Before, they lived in the cache directory: the first use merges that file in (see
//! [`migrate_legacy`]). `export` and `import` carry them to another machine as JSON. A note belongs to a checkout (its root path) and records the
//! declaration's identity (its language, namespace or module, qualified name, kind, and for C# its parameter types,
//! which tell overloads apart), its file, a hash of its body (each line's trailing whitespace dropped, so moving or
//! re-indenting the end of a line doesn't count, but editing does), the body itself (up to 64 KB, for a diff
//! summary), the commit, the author (`git config user.name`) and the time.
//!
//! Each time notes are read they are matched to the index again, by identity: a declaration with the same identity
//! whose body hashes the same is `current` (in another file: re-attached, `moved_from` says where it was); one whose
//! body differs is `stale`, with the first later commit whose copy of the file no longer holds the old body and the
//! size of the change; none is `orphaned`, with the declarations it may have become listed as `candidates`, until
//! one with its identity appears again. A same-named declaration of another namespace never takes a note over, even
//! with an identical body.
//!
//! Notes written before identities were recorded keep everything they had; the first time one is read while its
//! declaration is still in its file, its identity is recorded from it. Until then it says `identity: unknown` and
//! attaches only to a declaration of its name and kind in its own file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kc_store::Store;
use kc_store::query::{self, SymbolRow};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// Tags a note may carry.
pub const TAGS: &[&str] = &["decision", "gotcha", "todo"];
/// Bodies longer than this are stored without their text (the hash still marks them stale, without a diff summary).
const MAX_BODY: usize = 64 * 1024;
/// Commits searched for the one that changed a stale note's symbol.
const MAX_STALE_COMMITS: usize = 20;

// Never dropped: a new column is added with `ALTER TABLE`, so no upgrade loses notes.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS notes (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    repo      TEXT NOT NULL,
    symbol    TEXT NOT NULL,
    kind      TEXT NOT NULL,
    file      TEXT NOT NULL,
    line      INTEGER NOT NULL,
    body_hash TEXT NOT NULL,
    body      TEXT,
    commit_sha TEXT,
    author    TEXT,
    created   INTEGER NOT NULL,
    tag       TEXT,
    text      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS notes_repo ON notes(repo, symbol);
";

/// Columns added since the first schema, with their types: added in place, never by recreating the table. A note
/// from before has them NULL: its identity isn't known.
const ADDED_COLUMNS: &[(&str, &str)] = &[("lang", "TEXT"), ("namespace", "TEXT"), ("params", "TEXT")];

/// Adds the columns of [`ADDED_COLUMNS`] a database lacks. Every note is kept; one from before has no identity.
fn migrate(conn: &Connection) -> Result<()> {
    let have: Vec<String> = conn.prepare("PRAGMA table_info(notes)")?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    for (col, ty) in ADDED_COLUMNS {
        if !have.iter().any(|h| h == col) {
            conn.execute_batch(&format!("ALTER TABLE notes ADD COLUMN {col} {ty}"))?;
        }
    }
    Ok(())
}

/// The notes database (see [`crate::config::notes_path`]): in the user data directory, never the cache.
pub fn db_path() -> Result<PathBuf> {
    Ok(crate::config::notes_path()?.0)
}

/// Where notes lived before they moved out of the cache: `<cache>/notes.sqlite`.
pub fn legacy_path() -> Result<PathBuf> {
    Ok(crate::cache_dir()?.join(crate::config::NOTES_FILE))
}

/// Explicit uninstall consent: clear notes in current and legacy databases, including migration backups. Keep
/// database files and schemas so another client's connection is never left attached to an unlinked database.
/// SQLite transactions coordinate with live writers; unrelated files are never opened or removed.
pub fn clear_all() -> Result<Vec<(PathBuf, usize)>> {
    let legacy = legacy_path()?;
    let mut paths = vec![db_path()?, legacy.clone()];
    if let Some(dir) = legacy.parent() && let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "notes.sqlite.migrated" || name.strip_prefix("notes.sqlite.migrated.")
                .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
            {
                paths.push(entry.path());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut cleared = Vec::new();
    for path in paths {
        if !path.exists() { continue; }
        if !seen.insert(std::fs::canonicalize(&path)?) { continue; }
        let mut conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .with_context(|| format!("opening notes {}; retained", path.display()))?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .with_context(|| format!("notes {} are in use; retry later", path.display()))?;
        let count = tx.execute("DELETE FROM notes", [])
            .with_context(|| format!("clearing notes {}; not a recognized notes database or unavailable", path.display()))?;
        tx.commit()?;
        cleared.push((path, count));
    }
    Ok(cleared)
}

fn open_db(create: bool) -> Result<Option<Connection>> {
    let path = db_path()?;
    // A note is never lost to a failed move: the old file stays where it was, read again next time.
    if let Err(e) = migrate_legacy(&path)
        && !MIGRATE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        eprintln!("knowcode: notes in the cache directory were not moved to {}: {e:#}", path.display());
    }
    if !create && !path.exists() {
        return Ok(None);
    }
    if create {
        outside_repos(&path)?;
    }
    Ok(Some(open_at(&path)?))
}

/// Refuses a notes database inside an indexed repo (`notes.path` pointed there): nothing is written into a repo.
fn outside_repos(path: &Path) -> Result<()> {
    let path = canonical(path);
    // Each enclosing checkout with an index (a worktree may sit inside another checkout).
    for root in path.ancestors().skip(1).filter(|d| d.join(".git").exists()) {
        if crate::db_path(root)?.exists() {
            bail!("notes database {} is inside the indexed repo {}: set notes.path outside it", path.display(), root.display());
        }
    }
    Ok(())
}

static MIGRATE_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Opens (creating) the notes database at `path`, its schema brought up to date.
fn open_at(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// `path` with its directory's links resolved, so two spellings of one file compare equal before it exists.
fn canonical(path: &Path) -> PathBuf {
    match (path.parent().and_then(|d| std::fs::canonicalize(d).ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// Moves the notes of the old location, `<cache>/notes.sqlite`, into the database at `path`: every note not already
/// there (the same repo, symbol, kind, creation time, text and body hash) is added, keeping its id unless another
/// note has it; then the old file is renamed `notes.sqlite.migrated` (`.migrated.1`, … when that name is taken), never
/// deleted. Nothing happens when there's no old file or `notes.path` names it. Merging is idempotent, so two
/// processes migrating at once, or an older build writing the old file again, add nothing twice. Returns the notes
/// added and already present, when there was a file to move.
pub fn migrate_legacy(path: &Path) -> Result<Option<Imported>> {
    let legacy = legacy_path()?;
    if !legacy.is_file() || canonical(&legacy) == canonical(path) {
        return Ok(None);
    }
    outside_repos(path)?;
    let records = records(&open_at(&legacy)?, None)?;
    let imported = insert(&mut open_at(path)?, &records)?;
    let mut to = PathBuf::from(format!("{}.migrated", legacy.display()));
    for n in 1.. {
        if !to.exists() {
            break;
        }
        to = PathBuf::from(format!("{}.migrated.{n}", legacy.display()));
    }
    match std::fs::rename(&legacy, &to) {
        Ok(()) => eprintln!(
            "knowcode: moved notes from the cache to {} ({} added, {} already there); the old file is {}",
            path.display(),
            imported.added,
            imported.present,
            to.display()
        ),
        // Another process moved it first.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("renaming {}", legacy.display())),
    }
    Ok(Some(imported))
}

/// How many notes there are, in every repo.
pub fn count() -> Result<usize> {
    let Some(conn) = open_db(false)? else { return Ok(0) };
    Ok(conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))? as usize)
}

/// The `format` of an export file.
pub const EXPORT_FORMAT: &str = "knowcode-notes";
/// The `version` of the export format this build writes; it reads this one and older.
pub const EXPORT_VERSION: u32 = 1;

/// Notes as `note export` writes them and `note import` reads them: every stored field, so a note moved to another
/// machine keeps its identity, its body hash (current or stale) and its history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Export {
    pub format: String,
    pub version: u32,
    pub notes: Vec<Record>,
}

/// One stored note, whole. Fields in a fixed order; absent values are `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: i64,
    /// The checkout's root path.
    pub repo: String,
    /// The declaration's qualified name when the note was added.
    pub qualified: String,
    pub kind: String,
    /// The rest of its identity; `null` for a note written before identities were recorded.
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub param_types: Option<String>,
    pub file: String,
    pub line: u32,
    pub body_hash: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Unix seconds.
    pub created: i64,
    #[serde(default)]
    pub tag: Option<String>,
    pub text: String,
}

/// What an import added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Imported {
    pub added: usize,
    /// Already there: the same repo, symbol, kind, creation time, text and body hash.
    pub present: usize,
    /// Added under a new id, another note having theirs.
    pub renumbered: usize,
}

fn records(conn: &Connection, repo: Option<&str>) -> Result<Vec<Record>> {
    let mut stmt = conn.prepare(
        "SELECT id, repo, symbol, kind, lang, namespace, params, file, line, body_hash, body, commit_sha, author, created, tag, text
         FROM notes WHERE ?1 IS NULL OR repo = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([repo], |r| {
        Ok(Record {
            id: r.get(0)?,
            repo: r.get(1)?,
            qualified: r.get(2)?,
            kind: r.get(3)?,
            lang: r.get(4)?,
            namespace: r.get(5)?,
            param_types: r.get(6)?,
            file: r.get(7)?,
            line: r.get(8)?,
            body_hash: r.get(9)?,
            body: r.get(10)?,
            commit: r.get(11)?,
            author: r.get(12)?,
            created: r.get(13)?,
            tag: r.get(14)?,
            text: r.get(15)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Adds the records not already stored (see [`Imported::present`]), in one transaction, each keeping its id when it
/// is free.
fn insert(conn: &mut Connection, records: &[Record]) -> Result<Imported> {
    let tx = Transaction::new(conn, TransactionBehavior::Immediate)?;
    let mut out = Imported { added: 0, present: 0, renumbered: 0 };
    let taken = |tx: &Transaction, id: i64| -> Result<bool> { Ok(id <= 0 || tx.query_row("SELECT 1 FROM notes WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some()) };
    // Those whose id is free go first, so a renumbered note's new id never takes one an incoming note has.
    let (mut keep, mut renumber) = (Vec::new(), Vec::new());
    for r in records {
        let present = tx
            .query_row(
                "SELECT 1 FROM notes WHERE repo = ?1 AND symbol = ?2 AND kind = ?3 AND created = ?4 AND text = ?5 AND body_hash = ?6",
                params![r.repo, r.qualified, r.kind, r.created, r.text, r.body_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if present {
            out.present += 1;
        } else if taken(&tx, r.id)? {
            renumber.push(r);
        } else {
            keep.push(r);
        }
    }
    let mut queue: Vec<(&Record, bool)> = keep.into_iter().map(|r| (r, false)).collect();
    queue.extend(renumber.into_iter().map(|r| (r, true)));
    for (r, renumbered) in queue {
        // An export edited by hand may repeat an id.
        let new_id = renumbered || taken(&tx, r.id)?;
        tx.execute(
            "INSERT INTO notes(id, repo, symbol, kind, file, line, body_hash, body, commit_sha, author, created, tag, text, lang, namespace, params)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                (!new_id).then_some(r.id),
                r.repo,
                r.qualified,
                r.kind,
                r.file,
                r.line,
                r.body_hash,
                r.body,
                r.commit,
                r.author,
                r.created,
                r.tag,
                r.text,
                r.lang,
                r.namespace,
                r.param_types
            ],
        )?;
        out.added += 1;
        out.renumbered += usize::from(new_id);
    }
    tx.commit()?;
    Ok(out)
}

/// Every note, or those of the repo at `repo`, whole, for `note export`.
pub fn export(repo: Option<&Path>) -> Result<Export> {
    let notes = match open_db(false)? {
        Some(conn) => records(&conn, repo.map(repo_key).as_deref())?,
        None => Vec::new(),
    };
    Ok(Export { format: EXPORT_FORMAT.to_string(), version: EXPORT_VERSION, notes })
}

/// Adds the notes of an export not already stored (see [`Imported::present`]): importing a file twice adds nothing
/// the second time. With `repo`, the notes (of one repo) are put under the repo at that path: the checkout is
/// elsewhere on this machine.
pub fn import(export: &Export, repo: Option<&Path>) -> Result<Imported> {
    let records = checked(export, repo)?;
    let mut conn = open_db(true)?.expect("created");
    insert(&mut conn, &records)
}

/// The export's records, checked, with their repo replaced by `repo` when given.
fn checked(export: &Export, repo: Option<&Path>) -> Result<Vec<Record>> {
    if export.format != EXPORT_FORMAT {
        bail!("not a knowcode notes export (format {:?}, expected {EXPORT_FORMAT:?})", export.format);
    }
    if export.version > EXPORT_VERSION {
        bail!("notes export version {} is newer than this build reads ({EXPORT_VERSION}): upgrade knowcode", export.version);
    }
    if let Some(n) = export.notes.iter().position(|r| r.text.trim().is_empty() || r.qualified.is_empty() || r.repo.is_empty()) {
        bail!("note {} of the export has no text, symbol or repo", n + 1);
    }
    let mut records = export.notes.clone();
    if let Some(repo) = repo {
        let repos: std::collections::BTreeSet<&str> = records.iter().map(|r| r.repo.as_str()).collect();
        if repos.len() > 1 {
            bail!("the export holds notes of {} repos; --repo moves the notes of one (export each with --repo)", repos.len());
        }
        records.iter_mut().for_each(|r| r.repo = repo_key(repo));
    }
    Ok(records)
}

fn repo_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

/// A stored note.
#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub id: i64,
    /// The symbol's qualified name when the note was added.
    pub symbol: String,
    pub kind: String,
    /// Where the symbol was.
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub text: String,
    /// HEAD when the note was added.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// When it was added, `YYYY-MM-DD`.
    pub date: String,
    #[serde(skip)]
    pub created: i64,
    #[serde(skip)]
    pub hash: String,
    #[serde(skip)]
    pub body: Option<String>,
    /// The namespace or module of the declaration (see `kc_store::identity`), when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// The rest of its identity; `None` for a note written before identities were recorded, not yet established.
    #[serde(skip)]
    pub identity: Option<Identity>,
}

/// What identifies a declaration besides its qualified name and kind (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub lang: String,
    /// Empty when it has none.
    pub namespace: String,
    /// C#: its parameters' declared types, which tell overloads apart; empty elsewhere.
    pub params: String,
}

impl Identity {
    fn of(store: &Store, s: &SymbolRow) -> Result<Identity> {
        let conn = store.conn();
        let (lang, params): (String, Option<String>) = conn.query_row(
            "SELECT f.lang, s.param_types FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
            [s.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let namespace = match &s.namespace {
            Some(ns) => ns.clone(),
            None => kc_store::identity::namespace_of(conn, s)?.unwrap_or_default(),
        };
        Ok(Identity { lang, namespace, params: params.unwrap_or_default() })
    }
}

/// A note matched to the index as it is now.
#[derive(Debug, Clone, Serialize)]
pub struct NoteView {
    #[serde(flatten)]
    pub note: Note,
    /// `current`, `stale` (the symbol's body changed) or `orphaned` (no symbol has its name).
    pub status: &'static str,
    /// Where the symbol is now, when it is found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now_at: Option<String>,
    /// Re-attached: the symbol is now in another file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    /// A stale note's first commit after the note whose copy of the file no longer holds the noted body; `working
    /// tree` when no commit changed it (uncommitted edits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_since: Option<String>,
    /// A stale note's change, as lines added and removed: `+3 −1 lines`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    /// `unknown` for a note written before identities were recorded whose declaration couldn't be found to record
    /// it: it attaches only to a declaration of its name and kind in its own file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<&'static str>,
    /// An orphaned note's possible new homes, `file:line namespace.Name`: declarations of its name with another
    /// identity, or with its body. Never attached by themselves.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// How many candidates there are, when more than [`CANDIDATES_SHOWN`] are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates_total: Option<usize>,
}

/// At most this many candidates are listed for an orphaned note.
pub const CANDIDATES_SHOWN: usize = 10;

impl NoteView {
    /// One line for an answer's warnings: `note on X (stale since abc123): text`.
    pub fn line(&self) -> String {
        let status = match self.status {
            "stale" => format!(" (stale since {})", self.stale_since.as_deref().unwrap_or("the note")),
            "orphaned" if self.candidates.is_empty() => " (orphaned)".to_string(),
            "orphaned" => {
                let more = self.candidates_total.map(|t| format!(" and {} more", t - self.candidates.len())).unwrap_or_default();
                format!(" (orphaned; candidates {}{more})", self.candidates.join(", "))
            }
            _ => String::new(),
        };
        let tag = self.note.tag.as_deref().map(|t| format!("[{t}] ")).unwrap_or_default();
        format!("note #{} on {}{status}: {tag}{}", self.note.id, self.note.symbol, self.note.text)
    }
}

/// A symbol's body as notes hash it: its source bytes, each line's trailing whitespace dropped.
fn body_of(root: &Path, s: &SymbolRow) -> Option<String> {
    let bytes = std::fs::read(root.join(&s.file)).ok()?;
    let body = bytes.get(s.start_byte..s.end_byte.min(bytes.len()))?;
    Some(normalize(&String::from_utf8_lossy(body)))
}

fn normalize(text: &str) -> String {
    text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")
}

fn hash(body: &str) -> String {
    blake3::hash(body.as_bytes()).to_hex()[..16].to_string()
}

/// The definition `query` names (`Type.member`, a name), as `context` picks it: the first.
fn resolve(store: &Store, query: &str) -> Result<SymbolRow> {
    let mut defs = query::definitions(store.conn(), query)?;
    if defs.is_empty() {
        bail!("no definition named {query}");
    }
    Ok(defs.remove(0))
}

/// Adds a note to the symbol `symbol` names in the repo at `root` (synced by the caller).
pub fn add(root: &Path, store: &Store, symbol: &str, text: &str, tag: Option<&str>) -> Result<NoteView> {
    let text = text.trim();
    if text.is_empty() {
        bail!("a note needs some text");
    }
    if let Some(t) = tag
        && !TAGS.contains(&t)
    {
        bail!("unknown tag {t}; one of {}", TAGS.join(", "));
    }
    let s = resolve(store, symbol)?;
    let identity = Identity::of(store, &s)?;
    let body = body_of(root, &s).with_context(|| format!("reading {}", s.file))?;
    let commit = crate::git::GitDir::find(root).and_then(|g| g.head_commit());
    let author = crate::git::run(root, &["config", "user.name"])
        .ok()
        .map(|o| String::from_utf8_lossy(&o).trim().to_string())
        .filter(|a| !a.is_empty())
        .or_else(|| std::env::var("USER").ok());
    let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let conn = open_db(true)?.expect("created");
    conn.execute(
        "INSERT INTO notes(repo, symbol, kind, file, line, body_hash, body, commit_sha, author, created, tag, text, lang, namespace, params)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            repo_key(root),
            s.qualified(),
            s.kind,
            s.file,
            s.line,
            hash(&body),
            (body.len() <= MAX_BODY).then_some(&body),
            commit,
            author,
            created,
            tag,
            text,
            identity.lang,
            identity.namespace,
            identity.params
        ],
    )?;
    let id = conn.last_insert_rowid();
    let note = read(&conn, "WHERE id = ?1", params![id])?.into_iter().next().context("note just added")?;
    view(root, store, &conn, note)
}

/// Removes note `id` of the repo at `root`. Returns whether there was one.
pub fn remove(root: &Path, id: i64) -> Result<bool> {
    let Some(conn) = open_db(false)? else { return Ok(false) };
    Ok(conn.execute("DELETE FROM notes WHERE id = ?1 AND repo = ?2", params![id, repo_key(root)])? > 0)
}

/// Removes the notes of the repo at `root` anchored in files `blocked` says its AI privacy policy blocks (their text
/// and recorded bodies are of blocked code), overwriting what they held on disk. Returns how many.
pub fn purge(root: &Path, blocked: impl Fn(&str) -> bool) -> Result<usize> {
    let Some(conn) = open_db(false)? else { return Ok(0) };
    let rows: Vec<(i64, String)> =
        conn.prepare("SELECT id, file FROM notes WHERE repo = ?1")?.query_map([repo_key(root)], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let gone: Vec<i64> = rows.into_iter().filter(|(_, f)| blocked(f)).map(|(id, _)| id).collect();
    if gone.is_empty() {
        return Ok(0);
    }
    // Deleted rows are zeroed, not left in free pages, and the WAL is emptied into the file.
    conn.execute_batch("PRAGMA secure_delete = ON;")?;
    let tx = conn.unchecked_transaction()?;
    for id in &gone {
        tx.execute("DELETE FROM notes WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(gone.len())
}

fn read(conn: &Connection, filter: &str, args: impl rusqlite::Params) -> Result<Vec<Note>> {
    let sql = format!(
        "SELECT id, symbol, kind, file, line, body_hash, body, commit_sha, author, created, tag, text, lang, namespace, params
         FROM notes {filter} ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args, |r| {
        let created: i64 = r.get(9)?;
        Ok(Note {
            id: r.get(0)?,
            symbol: r.get(1)?,
            kind: r.get(2)?,
            file: r.get(3)?,
            line: r.get(4)?,
            hash: r.get(5)?,
            body: r.get(6)?,
            commit: r.get(7)?,
            author: r.get(8)?,
            created,
            date: crate::vitals::format_date(created),
            tag: r.get(10)?,
            text: r.get(11)?,
            namespace: r.get::<_, Option<String>>(13)?.filter(|n| !n.is_empty()),
            identity: match r.get::<_, Option<String>>(12)? {
                Some(lang) => Some(Identity {
                    lang,
                    namespace: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
                    params: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
                }),
                None => None,
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every note of the repo at `root`, matched to its index.
pub fn all(root: &Path, store: &Store) -> Result<Vec<NoteView>> {
    let Some(conn) = open_db(false)? else { return Ok(Vec::new()) };
    read(&conn, "WHERE repo = ?1", params![repo_key(root)])?.into_iter().map(|n| view(root, store, &conn, n)).collect()
}

/// Whether the repo at `root` has any note: a cheap check before matching notes to an answer's symbols.
pub fn any(root: &Path) -> Result<bool> {
    let Some(conn) = open_db(false)? else { return Ok(false) };
    Ok(conn.query_row("SELECT 1 FROM notes WHERE repo = ?1 LIMIT 1", [repo_key(root)], |_| Ok(())).optional()?.is_some())
}

/// The notes on symbols with these qualified names, matched to the index.
pub fn on(root: &Path, store: &Store, qualified: &[String]) -> Result<Vec<NoteView>> {
    if qualified.is_empty() {
        return Ok(Vec::new());
    }
    let Some(conn) = open_db(false)? else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for q in qualified {
        if !seen.insert(q) {
            continue;
        }
        for n in read(&conn, "WHERE repo = ?1 AND symbol = ?2", params![repo_key(root), q])? {
            out.push(view(root, store, &conn, n)?);
        }
    }
    out.sort_by_key(|v| v.note.id);
    Ok(out)
}

/// The notes on these declarations: those attached to one of them, and orphaned ones of their qualified name and
/// identity.
pub fn on_symbols(root: &Path, store: &Store, symbols: &[SymbolRow]) -> Result<Vec<NoteView>> {
    let names: Vec<String> = symbols.iter().map(SymbolRow::qualified).collect();
    let mut identities = Vec::new();
    for s in symbols {
        identities.push(Identity::of(store, s)?);
    }
    let mut out = on(root, store, &names)?;
    out.retain(|v| match &v.now_at {
        Some(at) => symbols.iter().any(|s| *at == format!("{}:{}", s.file, s.line)),
        None => v.note.identity.as_ref().is_none_or(|id| identities.contains(id)),
    });
    Ok(out)
}

/// The notes on what `query` names: those on its definitions, and, by name, orphaned ones (an orphaned note's
/// declaration is no longer there).
pub fn named(root: &Path, store: &Store, query: &str) -> Result<Vec<NoteView>> {
    let defs = query::definitions(store.conn(), query)?;
    let mut out = on_symbols(root, store, &defs)?;
    for v in on(root, store, &[query.trim().to_string()])? {
        if v.status == "orphaned" && !out.iter().any(|o| o.note.id == v.note.id) {
            out.push(v);
        }
    }
    out.sort_by_key(|v| v.note.id);
    Ok(out)
}

/// Matches a note to the index: among the declarations with its identity (see the module docs), the one whose body
/// hashes the same, else the one in its file, else the first. A note without a recorded identity gets it from the
/// declaration of its name and kind in its file, when there is exactly one, and is attached only there.
fn view(root: &Path, store: &Store, conn: &Connection, mut note: Note) -> Result<NoteView> {
    let named: Vec<SymbolRow> =
        query::definitions(store.conn(), &note.symbol)?.into_iter().filter(|d| d.qualified() == note.symbol && d.kind == note.kind).collect();
    let mut identities = Vec::with_capacity(named.len());
    for d in &named {
        identities.push(Identity::of(store, d)?);
    }
    let mut unknown = false;
    if note.identity.is_none() {
        let here: Vec<usize> = (0..named.len()).filter(|&i| named[i].file == note.file).collect();
        if let [i] = here[..] {
            let id = identities[i].clone();
            conn.execute(
                "UPDATE notes SET lang = ?2, namespace = ?3, params = ?4 WHERE id = ?1",
                params![note.id, id.lang, id.namespace, id.params],
            )?;
            note.namespace = Some(id.namespace.clone()).filter(|n| !n.is_empty());
            note.identity = Some(id);
        } else {
            unknown = true;
        }
    }
    let defs: Vec<&SymbolRow> = match &note.identity {
        Some(id) => named.iter().zip(&identities).filter(|(_, i)| *i == id).map(|(d, _)| d).collect(),
        None => named.iter().filter(|d| d.file == note.file).collect(),
    };
    let bodies: Vec<Option<String>> = defs.iter().map(|d| body_of(root, d)).collect();
    let pick = defs
        .iter()
        .zip(&bodies)
        .position(|(_, b)| b.as_deref().map(hash).as_deref() == Some(note.hash.as_str()))
        .or_else(|| defs.iter().position(|d| d.file == note.file))
        .or(if defs.is_empty() { None } else { Some(0) });
    let identity = unknown.then_some("unknown");
    let Some(i) = pick else {
        let (candidates, total) = candidates(root, store, &note, &named)?;
        let candidates_total = (total > candidates.len()).then_some(total);
        return Ok(NoteView { note, status: "orphaned", now_at: None, moved_from: None, stale_since: None, change: None, identity, candidates, candidates_total });
    };
    let d = defs[i];
    let body = bodies[i].clone().unwrap_or_default();
    let current = hash(&body) == note.hash;
    let moved_from = (d.file != note.file).then(|| note.file.clone());
    let (stale_since, change) = if current {
        (None, None)
    } else {
        (Some(stale_since(root, &note, &d.file)), note.body.as_deref().map(|old| change_summary(old, &body)))
    };
    Ok(NoteView {
        status: if current { "current" } else { "stale" },
        now_at: Some(format!("{}:{}", d.file, d.line)),
        moved_from,
        stale_since,
        change,
        identity,
        candidates: Vec::new(),
        candidates_total: None,
        note,
    })
}

/// Where an orphaned note's declaration may have gone, `file:line namespace.Name`: declarations of its qualified name
/// and kind (another identity), and declarations of its name whose body is the noted one. At most 10.
fn candidates(root: &Path, store: &Store, note: &Note, named: &[SymbolRow]) -> Result<(Vec<String>, usize)> {
    let bare = query::split_dotted(&note.symbol).1;
    let same_body: Vec<SymbolRow> = query::definitions(store.conn(), bare)?
        .into_iter()
        .filter(|d| !named.iter().any(|n| n.id == d.id) && body_of(root, d).as_deref().map(hash).as_deref() == Some(note.hash.as_str()))
        .collect();
    let mut out = Vec::new();
    for d in named.iter().chain(&same_body).take(CANDIDATES_SHOWN) {
        let ns = match &d.namespace {
            Some(ns) => Some(ns.clone()),
            None => kc_store::identity::namespace_of(store.conn(), d)?,
        };
        let name = ns.map_or_else(|| d.qualified(), |ns| format!("{ns}.{}", d.qualified()));
        out.push(format!("{}:{} {name}", d.file, d.line));
    }
    Ok((out, named.len() + same_body.len()))
}

/// The first commit after the note's whose copy of `file` no longer holds the noted body (short sha), searching
/// the oldest [`MAX_STALE_COMMITS`] commits touching the file; without the body, the first of them; `working tree`
/// when no commit changed it.
fn stale_since(root: &Path, note: &Note, file: &str) -> String {
    let Some(commit) = note.commit.as_deref() else { return "working tree".to_string() };
    let log = crate::git::run(root, &["log", "--format=%h", "--reverse", &format!("{commit}..HEAD"), "--", file]).unwrap_or_default();
    let commits: Vec<String> = String::from_utf8_lossy(&log).lines().map(str::to_string).collect();
    let Some(first) = commits.first() else { return "working tree".to_string() };
    let Some(body) = note.body.as_deref() else { return first.clone() };
    for c in commits.iter().take(MAX_STALE_COMMITS) {
        let Ok(text) = crate::git::run(root, &["show", &format!("{c}:{file}")]) else { return c.clone() };
        if !normalize(&String::from_utf8_lossy(&text)).contains(body) {
            return c.clone();
        }
    }
    match commits.get(MAX_STALE_COMMITS) {
        Some(c) => format!("{c} or later"),
        None => "working tree".to_string(),
    }
}

/// `+a −r lines`: lines added and removed between two bodies (a longest-common-subsequence diff; over 2,000 lines
/// each, just the line counts).
fn change_summary(old: &str, new: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    if a.len() > 2000 || b.len() > 2000 {
        return format!("{} → {} lines", a.len(), b.len());
    }
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let common = lcs[0][0] as usize;
    format!("+{} −{} lines", b.len() - common, a.len() - common)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_summary_counts_lines_added_and_removed() {
        assert_eq!(change_summary("a\nb\nc", "a\nB\nc\nd"), "+2 −1 lines");
        assert_eq!(change_summary("a", "a"), "+0 −0 lines");
    }

    #[test]
    fn opening_an_older_notes_database_adds_the_identity_columns_and_keeps_every_note() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute("INSERT INTO notes(repo, symbol, kind, file, line, body_hash, created, text) VALUES ('/r', 'A.b', 'method', 'a.cs', 1, 'h', 0, 't')", [])
            .unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let notes = read(&conn, "", []).unwrap();
        assert_eq!((notes.len(), notes[0].text.as_str(), notes[0].identity.is_none()), (1, "t", true));
    }

    fn db_with(rows: &[(&str, &str, i64)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        for (symbol, text, created) in rows {
            conn.execute(
                "INSERT INTO notes(repo, symbol, kind, file, line, body_hash, created, text, lang, namespace, params)
                 VALUES ('/r', ?1, 'method', 'a.cs', 1, 'h', ?3, ?2, 'csharp', 'N', 'int')",
                params![symbol, text, created],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn merging_notes_keeps_ids_adds_nothing_twice_and_renumbers_only_on_a_clash() {
        let from = records(&db_with(&[("A.b", "one", 1), ("A.c", "two", 2)]), None).unwrap();
        let mut to = db_with(&[]);
        assert_eq!(insert(&mut to, &from).unwrap(), Imported { added: 2, present: 0, renumbered: 0 });
        assert_eq!(records(&to, None).unwrap(), from, "a fresh database gets every field and id as they were");
        assert_eq!(insert(&mut to, &from).unwrap(), Imported { added: 0, present: 2, renumbered: 0 });
        // Both databases had notes: id 1 is taken by another note, so the incoming one gets a new id.
        let mut both = db_with(&[("X.y", "mine", 5)]);
        assert_eq!(insert(&mut both, &from).unwrap(), Imported { added: 2, present: 0, renumbered: 1 });
        let texts: Vec<(i64, String)> = records(&both, None).unwrap().into_iter().map(|r| (r.id, r.text)).collect();
        assert_eq!(texts, vec![(1, "mine".into()), (2, "two".into()), (3, "one".into())]);
        assert_eq!(insert(&mut both, &from).unwrap().added, 0);
    }

    #[test]
    fn an_export_round_trips_through_json_and_is_checked_on_import() {
        let export = Export { format: EXPORT_FORMAT.into(), version: EXPORT_VERSION, notes: records(&db_with(&[("A.b", "one", 1)]), None).unwrap() };
        let json = serde_json::to_string(&export).unwrap();
        for key in ["\"lang\":\"csharp\"", "\"namespace\":\"N\"", "\"qualified\":\"A.b\"", "\"kind\":\"method\"", "\"param_types\":\"int\""] {
            assert!(json.contains(key), "{key} in {json}");
        }
        let back: Export = serde_json::from_str(&json).unwrap();
        assert_eq!(back, export);
        assert_eq!(checked(&back, Some(Path::new("/moved"))).unwrap()[0].repo, "/moved");
        let newer = Export { version: EXPORT_VERSION + 1, ..back.clone() };
        assert!(format!("{:#}", checked(&newer, None).unwrap_err()).contains("newer"));
        let other = Export { format: "x".into(), ..back.clone() };
        assert!(checked(&other, None).is_err());
        let mut two = back.clone();
        two.notes.push(Record { repo: "/s".into(), ..back.notes[0].clone() });
        assert!(checked(&two, Some(Path::new("/moved"))).is_err(), "--repo moves one repo's notes, not several");
    }

    #[test]
    fn normalizing_drops_trailing_whitespace_only() {
        assert_eq!(normalize("a  \r\n  b\t\n"), "a\n  b");
        assert_ne!(hash(&normalize("x = 1")), hash(&normalize("x = 2")));
    }
}
