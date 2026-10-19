//! Storage for the content index: every searchable file (`cfiles`) and a trigram index over their bytes (`grams`).
//!
//! Each `grams` row maps one trigram (three bytes, ASCII-lowercased, packed into an integer) to the sorted ids of
//! the files containing it, delta-encoded as varints. The lists are a superset: a file whose content changed keeps
//! its old entries until the next rebuild, and ids of deleted files are skipped on read. Correctness only needs every
//! indexed file to be listed under each of its current trigrams, because every candidate is verified against the
//! file itself.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::{Batch, Store};

/// Content changed since the trigram lists were built: always a candidate until merged.
pub const PENDING: i64 = 0;
/// Listed under all of its trigrams.
pub const INDEXED: i64 = 1;
/// Not searched: binary, too large or generated (`skip` says which).
pub const SKIPPED: i64 = 2;

#[derive(Debug, Clone)]
pub struct CFile {
    pub id: i64,
    pub path: String,
    pub mtime_ns: i64,
    pub size: i64,
    pub hash: String,
    pub state: i64,
    pub skip: Option<String>,
}

impl Store {
    /// Every file the content index tracks, by id.
    pub fn cfiles(&self) -> Result<Vec<CFile>> {
        let mut stmt = self.conn.prepare_cached("SELECT id, path, mtime_ns, size, hash, state, skip FROM cfiles ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(CFile {
                id: r.get(0)?,
                path: r.get(1)?,
                mtime_ns: r.get(2)?,
                size: r.get(3)?,
                hash: r.get(4)?,
                state: r.get(5)?,
                skip: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The file ids listed under one trigram, ascending. `None` when no file has it.
    pub fn posting(&self, gram: u32) -> Result<Option<Vec<u32>>> {
        let blob: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT posts FROM grams WHERE gram = ?1")?
            .query_row([gram], |r| r.get(0))
            .optional()?;
        Ok(blob.map(|b| decode_posts(&b)))
    }

    /// Content coverage for `status`: searchable files, pending files, skipped files by reason, trigram count.
    pub fn content_stats(&self) -> Result<serde_json::Value> {
        let mut by: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        let mut stmt = self.conn.prepare("SELECT state, IFNULL(skip, ''), COUNT(*) FROM cfiles GROUP BY 1, 2")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
            let (state, skip, n) = row?;
            let key = match state {
                INDEXED => "indexed".to_string(),
                PENDING => "pending".to_string(),
                _ => format!("skipped_{skip}"),
            };
            *by.entry(key).or_default() += n;
        }
        let grams: i64 = self.conn.query_row("SELECT COUNT(*) FROM grams", [], |r| r.get(0))?;
        Ok(serde_json::json!({ "files": by, "trigrams": grams }))
    }

    /// Symbols of one file whose line span holds at least one of `lines` (ascending), for ranking search hits.
    pub fn file_symbols(&self, path: &str, lines: &[u32]) -> Result<Vec<FileSymbol>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.start_line, s.end_line, s.name, s.kind, s.container, s.tags
             FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1",
        )?;
        let mut rows = stmt.query([path])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let (start_line, end_line): (u32, u32) = (r.get(0)?, r.get(1)?);
            let first = lines.partition_point(|&l| l < start_line);
            if lines.get(first).is_none_or(|&l| l > end_line) {
                continue; // no hit inside: skip without decoding the strings
            }
            out.push(FileSymbol {
                name: r.get(2)?,
                kind: r.get(3)?,
                container: r.get(4)?,
                start_line,
                end_line,
                test: r.get_ref(5)?.as_str().is_ok_and(|t| t.split(',').any(|t| t == "test")),
            });
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct FileSymbol {
    pub name: String,
    pub kind: String,
    pub container: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub test: bool,
}

/// A content file's new state after a freshness check.
pub struct CFileUpdate<'a> {
    pub id: i64,
    pub mtime_ns: i64,
    pub size: i64,
    pub hash: &'a str,
    pub state: i64,
    pub skip: Option<&'a str>,
}

impl Batch<'_> {
    /// Makes `cfiles` list exactly `paths`: new paths are added as pending, missing ones removed.
    pub fn set_cfile_paths(&self, paths: &[String]) -> Result<()> {
        let mut existing: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = self.tx.prepare_cached("SELECT path, id FROM cfiles")?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (p, id) = row?;
                existing.insert(p, id);
            }
        }
        let mut insert = self.tx.prepare_cached("INSERT INTO cfiles(path) VALUES (?1)")?;
        for p in paths {
            if existing.remove(p).is_none() {
                insert.execute([p])?;
            }
        }
        let mut delete = self.tx.prepare_cached("DELETE FROM cfiles WHERE id = ?1")?;
        for id in existing.into_values() {
            delete.execute([id])?;
        }
        Ok(())
    }

    pub fn update_cfile(&self, u: &CFileUpdate) -> Result<()> {
        self.tx
            .prepare_cached("UPDATE cfiles SET mtime_ns = ?2, size = ?3, hash = ?4, state = ?5, skip = ?6 WHERE id = ?1")?
            .execute(params![u.id, u.mtime_ns, u.size, u.hash, u.state, u.skip])?;
        Ok(())
    }

    pub fn set_cfile_state(&self, id: i64, state: i64) -> Result<()> {
        self.tx.prepare_cached("UPDATE cfiles SET state = ?2 WHERE id = ?1")?.execute(params![id, state])?;
        Ok(())
    }

    /// Replaces every trigram list. `pairs` yields `gram << 32 | file id`, ascending.
    pub fn replace_grams(&self, pairs: impl IntoIterator<Item = u64>) -> Result<()> {
        self.tx.execute("DELETE FROM grams", [])?;
        let mut stmt = self.tx.prepare_cached("INSERT INTO grams(gram, posts) VALUES (?1, ?2)")?;
        for (g, ids) in runs(pairs) {
            stmt.execute(params![g, encode_posts(&ids)])?;
        }
        Ok(())
    }

    /// Adds file ids to trigram lists. `pairs` yields `gram << 32 | file id`, ascending.
    pub fn merge_grams(&self, pairs: impl IntoIterator<Item = u64>) -> Result<()> {
        let mut read = self.tx.prepare_cached("SELECT posts FROM grams WHERE gram = ?1")?;
        let mut write = self.tx.prepare_cached(
            "INSERT INTO grams(gram, posts) VALUES (?1, ?2) ON CONFLICT(gram) DO UPDATE SET posts = excluded.posts",
        )?;
        for (g, ids) in runs(pairs) {
            let old: Option<Vec<u8>> = read.query_row([g], |r| r.get(0)).optional()?;
            let merged = match old {
                Some(b) => union(&decode_posts(&b), &ids),
                None => ids,
            };
            write.execute(params![g, encode_posts(&merged)])?;
        }
        Ok(())
    }
}

/// Groups sorted `gram << 32 | id` pairs into (gram, ascending distinct ids).
fn runs(pairs: impl IntoIterator<Item = u64>) -> impl Iterator<Item = (u32, Vec<u32>)> {
    let mut pairs = pairs.into_iter().peekable();
    std::iter::from_fn(move || {
        let g = (*pairs.peek()? >> 32) as u32;
        let mut ids = Vec::new();
        while let Some(p) = pairs.next_if(|&p| (p >> 32) as u32 == g) {
            let id = p as u32;
            if ids.last() != Some(&id) {
                ids.push(id);
            }
        }
        Some((g, ids))
    })
}

/// The union of two ascending id lists.
pub fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// The intersection of two ascending id lists.
pub fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Ascending ids as varint deltas.
pub fn encode_posts(ids: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ids.len() + 4);
    let mut prev = 0u32;
    for &id in ids {
        let mut d = id - prev;
        prev = id;
        loop {
            let byte = (d & 0x7f) as u8;
            d >>= 7;
            if d == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }
    out
}

pub fn decode_posts(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    let (mut cur, mut shift, mut prev) = (0u32, 0, 0u32);
    for &b in bytes {
        cur |= ((b & 0x7f) as u32) << shift;
        if b & 0x80 == 0 {
            prev += cur;
            out.push(prev);
            cur = 0;
            shift = 0;
        } else {
            shift += 7;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts_round_trip() {
        let ids = vec![1, 2, 3, 130, 20_000, 3_000_000];
        assert_eq!(decode_posts(&encode_posts(&ids)), ids);
        assert_eq!(encode_posts(&[1, 2, 3]).len(), 3);
        assert_eq!(union(&[1, 3, 5], &[2, 3, 6]), vec![1, 2, 3, 5, 6]);
        assert_eq!(intersect(&[1, 3, 5, 7], &[3, 4, 7]), vec![3, 7]);
        let pairs = [1u64 << 32 | 4, 1 << 32 | 9, 2 << 32 | 4, 2 << 32 | 4];
        assert_eq!(runs(pairs).collect::<Vec<_>>(), vec![(1, vec![4, 9]), (2, vec![4])]);
    }
}
