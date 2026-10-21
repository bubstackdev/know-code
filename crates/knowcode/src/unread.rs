//! Files KnowCode lists but doesn't read as code (Razor markup, vendored or built JavaScript, a source file it skipped),
//! checked as text for a name before an answer says uses there may be missing: through the content index's trigram
//! lists, so only the files that can hold the name are read. An answer then names the lines that do, or says none do,
//! instead of a note that only says such files exist.

use std::collections::HashSet;

use anyhow::Result;
use kc_index::search::{self, Case, SearchOptions};
use kc_store::content::SKIPPED;

use crate::ops::Opened;

/// At most this many lines are read per check; past it the count is a lower bound.
const LINES_READ: usize = 50;
/// At most this many lines are named in a note.
pub const LINES_NAMED: usize = 3;

/// Which of a repo's files a check covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Files {
    /// Razor markup (`.razor`, `.cshtml`), which KnowCode doesn't parse.
    Razor,
    /// Every source file the symbol index doesn't hold: Razor, vendored or built JavaScript, a file over the size
    /// limit or one an ignore rule keeps out of the symbol index but not out of search.
    NotCode,
}

/// What a check found.
#[derive(Debug, Default, Clone)]
pub struct Mentions {
    /// Files of the kind checked.
    pub files: usize,
    /// Of them, those search can't read (binary, over 1 MB, generated): not checked.
    pub unsearchable: usize,
    /// Lines naming a name as a word, outside comments: `(path, line)`, in path order.
    pub lines: Vec<(String, u32)>,
    /// More lines than were read: `lines` is a lower bound.
    pub capped: bool,
}

impl Mentions {
    /// `path:line` for the first lines, and how many more.
    pub fn named(&self) -> String {
        let mut out: Vec<String> = self.lines.iter().take(LINES_NAMED).map(|(f, l)| format!("{f}:{l}")).collect();
        let more = self.lines.len().saturating_sub(LINES_NAMED);
        if more > 0 || self.capped {
            out.push(format!("{more}{} more", if self.capped { "+" } else { "" }));
        }
        out.join(", ")
    }
}

/// Razor markup by its extension.
pub fn is_razor(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".razor") || lower.ends_with(".cshtml")
}

/// A source file in a language whose uses KnowCode would list if it read it.
fn is_source(path: &str) -> bool {
    is_razor(path) || matches!(search::content_lang(path), "swift" | "python" | "rust" | "csharp" | "javascript" | "typescript" | "cfml")
}

/// `name` as a regex matching it literally.
fn escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Lines in `o`'s files of kind `which` that name any of `names` as a whole word (case-sensitive), outside comments.
/// Reads the content index as last synced; the files themselves are read as they are now.
pub fn mentions(o: &Opened, names: &[&str], which: Files) -> Result<Mentions> {
    let names: Vec<&str> = names.iter().copied().filter(|n| !n.is_empty()).collect();
    let mut out = Mentions::default();
    let conn = o.store.conn();
    let parsed: HashSet<String> = if which == Files::NotCode {
        let mut stmt = conn.prepare_cached("SELECT path FROM files")?;
        stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    } else {
        HashSet::new()
    };
    let keep = |path: &str| match which {
        Files::Razor => is_razor(path),
        Files::NotCode => is_source(path) && !parsed.contains(path),
    };
    let files: Vec<_> = o.store.cfiles()?.into_iter().filter(|f| keep(&f.path)).collect();
    out.files = files.len();
    out.unsearchable = files.iter().filter(|f| f.state == SKIPPED).count();
    if files.is_empty() || names.is_empty() {
        return Ok(out);
    }
    let pattern = names.iter().map(|n| escape(n)).collect::<Vec<_>>().join("|");
    let opts = SearchOptions { pattern, regex: true, case: Case::Sensitive, word: true, langs: Vec::new(), globs: Vec::new(), max_lines: LINES_READ };
    let m = search::compile(&opts)?;
    let (candidates, _) = search::candidates(&o.store, files, &m, 0)?;
    if candidates.is_empty() {
        return Ok(out);
    }
    let threads = kc_index::SyncOptions::default().threads;
    let scanned = search::scan(std::slice::from_ref(&o.root), &candidates, &m, threads, LINES_READ);
    for f in &scanned.files {
        out.lines.extend(f.lines.iter().filter(|h| !h.comment).map(|h| (f.path.clone(), h.line)));
    }
    out.capped = scanned.stats.capped;
    Ok(out)
}
