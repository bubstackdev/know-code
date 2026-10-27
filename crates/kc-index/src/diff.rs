//! Changed line ranges from `git diff`, for diff-based impact, and the definitions the change removes.
//!
//! Paths come from git's NUL-delimited raw output (`--raw -z`), never from parsing the patch's quoted headers, so a
//! name with a space, a tab, a quote or non-ASCII text is read exactly; the patch's hunks are attached to them by
//! matching each `diff --git` header against the header git writes for that pair.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::git;

#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub file: String,
    /// For a renamed (or moved) file, its path before the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    /// Changed line ranges in the new version, inclusive. A pure deletion is the line it happened after.
    pub ranges: Vec<(u32, u32)>,
    pub deleted: bool,
    /// The blob of the file before the change, when there was one.
    #[serde(skip)]
    pub old_blob: Option<String>,
    /// Whether the change removes or rewrites lines of the old version (a hunk with old lines), so a definition may
    /// be gone.
    #[serde(skip)]
    pub removes_lines: bool,
}

/// A diff against a commit: the commit's object id and the files changed since.
#[derive(Debug, Clone, Default)]
pub struct Diff {
    pub base: String,
    pub changes: Vec<FileChange>,
}

/// Changes between `rev` and the working tree (`rev` defaults to HEAD, so `None` means uncommitted changes).
pub fn changes(root: &Path, rev: Option<&str>) -> Result<Vec<FileChange>> {
    Ok(diff(root, rev)?.changes)
}

/// [`changes`], with the commit `rev` resolved to. The revision is resolved by [`git::resolve_commit`] first, so a
/// string from the user only ever reaches `git diff` as an object id.
pub fn diff(root: &Path, rev: Option<&str>) -> Result<Diff> {
    let base = git::resolve_commit(root, rev.unwrap_or("HEAD"))?;
    // Paths the repo's AI privacy file blocks are left out by git, and their old versions are never read.
    let privacy = crate::privacy::Privacy::load(root);
    let excludes = privacy.exclude_pathspecs();
    // Prefixes, quoting, renames, colour and external tools are set here, whatever the user's git config says.
    let mut args: Vec<&str> = vec![
        "-c",
        "core.quotePath=false",
        "diff",
        "--raw",
        "--patch",
        "-z",
        "-U0",
        "--no-abbrev",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        &base,
        "--",
    ];
    args.extend(excludes.iter().map(String::as_str));
    let out = git::run(root, &args)?;
    let mut changes = parse(&out);
    changes.retain(|c| !privacy.blocks(&c.file));
    for c in changes.iter_mut().filter(|c| c.renamed_from.as_deref().is_some_and(|f| privacy.blocks(f))) {
        c.renamed_from = None;
        c.old_blob = None;
    }
    Ok(Diff { base, changes })
}

/// One `--raw -z` record: status letter, old blob, and the old and new paths.
struct Raw {
    status: u8,
    old_blob: String,
    old: Vec<u8>,
    new: Vec<u8>,
}

/// Parses `git diff --raw --patch -z` output: the raw records (`:meta\0path\0[path\0]`), an empty field, then the
/// patch.
fn parse(out: &[u8]) -> Vec<FileChange> {
    let mut fields = out.split(|&b| b == 0);
    let mut raws: Vec<Raw> = Vec::new();
    let mut consumed = 0;
    while let Some(meta) = fields.next() {
        consumed += meta.len() + 1;
        let Some(meta) = meta.strip_prefix(b":") else {
            break;
        };
        // `:old_mode new_mode old_sha new_sha status[score]`
        let meta = String::from_utf8_lossy(meta);
        let parts: Vec<&str> = meta.split(' ').collect();
        let (Some(old_blob), Some(status)) = (parts.get(2), parts.get(4).and_then(|s| s.bytes().next())) else { break };
        let Some(old) = fields.next() else { break };
        consumed += old.len() + 1;
        let new = if matches!(status, b'R' | b'C') {
            let Some(new) = fields.next() else { break };
            consumed += new.len() + 1;
            new
        } else {
            old
        };
        raws.push(Raw { status, old_blob: old_blob.to_string(), old: old.to_vec(), new: new.to_vec() });
    }
    let patch = out.get(consumed.min(out.len())..).unwrap_or_default();
    // Each pair's header as git writes it, to the pairs with it (in order: a header may repeat for an unmerged path).
    let mut by_header: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    for (i, r) in raws.iter().enumerate().rev() {
        by_header.entry(header(&r.old, &r.new)).or_default().push(i);
    }
    let mut changes: Vec<FileChange> = raws
        .iter()
        .map(|r| {
            let zero = r.old_blob.bytes().all(|b| b == b'0');
            FileChange {
                file: String::from_utf8_lossy(if r.status == b'D' { &r.old } else { &r.new }).into_owned(),
                renamed_from: (r.status == b'R').then(|| String::from_utf8_lossy(&r.old).into_owned()),
                ranges: Vec::new(),
                deleted: r.status == b'D',
                old_blob: (!zero && r.status != b'A').then(|| r.old_blob.clone()),
                removes_lines: matches!(r.status, b'D'),
            }
        })
        .collect();
    let mut current: Option<usize> = None;
    for line in patch.split(|&b| b == b'\n') {
        if line.starts_with(b"diff --git ") {
            current = by_header.get_mut(line).and_then(Vec::pop);
        } else if let Some(h) = line.strip_prefix(b"@@ ") {
            let Some(i) = current else { continue };
            let h = String::from_utf8_lossy(h);
            // @@ -a,b +c,d @@
            let count = |t: &str| -> (u32, u32) {
                match t.split_once(',') {
                    Some((s, c)) => (s.parse().unwrap_or(0), c.parse().unwrap_or(0)),
                    None => (t.parse().unwrap_or(0), 1),
                }
            };
            let mut tokens = h.split(' ');
            let (Some(old), Some(new)) = (tokens.next().and_then(|t| t.strip_prefix('-')), tokens.next().and_then(|t| t.strip_prefix('+')))
            else {
                continue;
            };
            let f = &mut changes[i];
            if count(old).1 > 0 {
                f.removes_lines = true;
            }
            let (start, n) = count(new);
            let end = if n == 0 { start } else { start + n - 1 };
            f.ranges.push((start.max(1), end.max(1)));
        }
    }
    changes
}

/// The `diff --git a/<old> b/<new>` line git writes for a pair, each side C-quoted when it needs to be (with
/// `core.quotePath=false`: a control character, `"` or `\`).
fn header(old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut h = b"diff --git ".to_vec();
    h.extend(quote(&[b"a/", old].concat()));
    h.push(b' ');
    h.extend(quote(&[b"b/", new].concat()));
    h
}

fn quote(name: &[u8]) -> Vec<u8> {
    if !name.iter().any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\') {
        return name.to_vec();
    }
    let mut q = vec![b'"'];
    for &b in name {
        match b {
            0x07 => q.extend(b"\\a"),
            0x08 => q.extend(b"\\b"),
            b'\t' => q.extend(b"\\t"),
            b'\n' => q.extend(b"\\n"),
            0x0b => q.extend(b"\\v"),
            0x0c => q.extend(b"\\f"),
            b'\r' => q.extend(b"\\r"),
            b'"' => q.extend(b"\\\""),
            b'\\' => q.extend(b"\\\\"),
            b if b < 0x20 || b == 0x7f => q.extend(format!("\\{b:03o}").bytes()),
            b => q.push(b),
        }
    }
    q.push(b'"');
    q
}

/// A definition in the version of a changed file before the change.
#[derive(Debug, Clone, Serialize)]
pub struct OldDefinition {
    pub name: String,
    pub kind: &'static str,
    /// Dotted name of the enclosing type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Its file before the change.
    pub file: String,
    /// Its file now: `None` when the change deletes the file.
    #[serde(skip)]
    pub now: Option<String>,
    /// Its first line before the change.
    pub line: u32,
    /// The language of its file (`Lang::as_str`).
    #[serde(skip)]
    pub lang: &'static str,
}

/// Kinds of definition whose removal can break code naming them: types, members and functions. Others (SQL, Ansible,
/// Terraform, CI facts) aren't read from the old side.
const REMOVABLE_KINDS: &[&str] = &["class", "struct", "enum", "actor", "protocol", "typealias", "function", "method", "property", "case"];

/// The definitions in the pre-change version (at `diff.base`) of each file the diff deletes, renames or removes
/// lines from: what may be gone after the change. Read from git's object store with one `git cat-file --batch`,
/// and parsed with the same extractors indexing uses.
pub fn old_definitions(root: &Path, diff: &Diff) -> Result<Vec<OldDefinition>> {
    let wanted: Vec<(&FileChange, &str, kc_extract::Lang)> = diff
        .changes
        .iter()
        .filter(|c| c.removes_lines || c.renamed_from.is_some())
        .filter_map(|c| {
            let blob = c.old_blob.as_deref()?;
            let old = c.renamed_from.as_deref().unwrap_or(&c.file);
            let lang = kc_extract::Lang::from_path(old)?;
            (!lang.is_manifest()).then_some((c, blob, lang))
        })
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let input: String = wanted.iter().map(|(_, b, _)| format!("{b}\n")).collect();
    let out = git::run_with_input(root, &["cat-file", "--batch"], input.as_bytes())?;
    let mut defs = Vec::new();
    let mut at = 0;
    for (c, blob, lang) in wanted {
        // `<sha> <type> <size>\n<content>\n`, or `<sha> missing\n`.
        let Some(nl) = out[at..].iter().position(|&b| b == b'\n') else { break };
        let head = String::from_utf8_lossy(&out[at..at + nl]).into_owned();
        at += nl + 1;
        let mut parts = head.split(' ');
        if parts.next() != Some(blob) {
            break;
        }
        let (Some(_), Some(Ok(size))) = (parts.next(), parts.next().map(str::parse::<usize>)) else { continue };
        let Some(bytes) = out.get(at..at + size) else { break };
        at += size + 1;
        let old = c.renamed_from.clone().unwrap_or_else(|| c.file.clone());
        let facts = kc_extract::extract_file(lang, &old, &kc_extract::decode(bytes));
        for s in &facts.symbols {
            let kind = s.kind.as_str();
            if !REMOVABLE_KINDS.contains(&kind) {
                continue;
            }
            defs.push(OldDefinition {
                name: s.name.clone(),
                kind,
                container: s.container.clone(),
                file: old.clone(),
                now: (!c.deleted).then(|| c.file.clone()),
                line: s.start_line,
                lang: lang.as_str(),
            });
        }
    }
    Ok(defs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_and_patch() {
        let z = "0".repeat(40);
        let (a, b, c) = ("1".repeat(40), "2".repeat(40), "3".repeat(40));
        let mut out = Vec::new();
        for (meta, paths) in [
            (format!(":100644 100644 {a} {z} M"), vec!["A.swift"]),
            (format!(":100644 000000 {b} {z} D"), vec!["B.swift"]),
            (format!(":100644 100644 {c} {z} R087"), vec!["old dir/x.py", "new dir/x.py"]),
            (format!(":100644 100644 {a} {z} M"), vec!["tab\there.py"]),
        ] {
            out.extend(meta.bytes());
            out.push(0);
            for p in paths {
                out.extend(p.bytes());
                out.push(0);
            }
        }
        out.push(0);
        out.extend(
            b"diff --git a/A.swift b/A.swift\n--- a/A.swift\n+++ b/A.swift\n@@ -3 +3,2 @@ x\n-a\n+b\n+c\n@@ -10,2 +11,0 @@\n-x\n-y\n\
diff --git a/B.swift b/B.swift\ndeleted file mode 100644\n--- a/B.swift\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-1\n-2\n-3\n\
diff --git a/old dir/x.py b/new dir/x.py\nsimilarity index 87%\nrename from old dir/x.py\nrename to new dir/x.py\n--- a/old dir/x.py\t\n+++ b/new dir/x.py\t\n@@ -0,0 +5 @@\n+z\n\
diff --git \"a/tab\\there.py\" \"b/tab\\there.py\"\n--- \"a/tab\\there.py\"\n+++ \"b/tab\\there.py\"\n@@ -2,0 +3 @@\n+q\n",
        );
        let c = parse(&out);
        assert_eq!(c[0].file, "A.swift");
        assert_eq!(c[0].ranges, [(3, 4), (11, 11)]);
        assert!(c[0].removes_lines && c[0].old_blob.is_some());
        assert!(c[1].deleted && c[1].file == "B.swift" && c[1].removes_lines);
        assert_eq!((c[2].file.as_str(), c[2].renamed_from.as_deref()), ("new dir/x.py", Some("old dir/x.py")));
        assert_eq!(c[2].ranges, [(5, 5)]);
        assert!(!c[2].removes_lines);
        assert_eq!((c[3].file.as_str(), c[3].ranges.as_slice()), ("tab\there.py", &[(3, 3)][..]));
    }

    #[test]
    fn quotes_like_git() {
        assert_eq!(quote(b"a/plain name.py"), b"a/plain name.py");
        assert_eq!(quote("a/ünï.py".as_bytes()), "a/ünï.py".as_bytes());
        assert_eq!(quote(b"a/q\"x\\y\tz\x01"), b"\"a/q\\\"x\\\\y\\tz\\001\"");
    }
}
