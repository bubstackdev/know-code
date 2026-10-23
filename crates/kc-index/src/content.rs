//! The content index: a trigram index over every searchable file, kept fresh from the working tree.
//!
//! Discovery (in [`crate::sync`]) lists the files; [`sync_content`] stats them on each search, rereads the ones whose
//! mtime or size moved, and keeps the trigram lists current:
//!
//! - A changed file becomes *pending*: it is verified on every search without consulting the trigram lists, so a
//!   one-file edit costs nothing to index.
//! - Once more than [`PENDING_FILES`] files (or [`PENDING_BYTES`]) are pending, their trigrams are merged into the
//!   lists.
//! - After many merges, the lists are rebuilt from scratch to drop entries for content that no longer exists.
//!
//! Binary files (a NUL byte in the first 8,000 bytes, the rule git uses), files over 1 MB and generated files
//! (lockfiles, minified files, files marked `@generated` or `DO NOT EDIT`) are tracked but never searched.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;
use kc_store::Store;
use kc_store::content::{CFile, CFileUpdate, INDEXED, PENDING, SKIPPED};
use serde::Serialize;

use crate::{MAX_CFML_BYTES, MAX_FILE_BYTES};

/// The largest file of `path` searched: [`MAX_FILE_BYTES`], or for a ColdFusion page the larger cap it is extracted
/// up to, so a page too large for other languages is still searchable.
pub fn max_search_bytes(path: &str) -> u64 {
    if kc_extract::cfml::is_cfml_path(path) { MAX_CFML_BYTES } else { MAX_FILE_BYTES }
}

/// Pending files are merged into the trigram lists past this many files...
pub const PENDING_FILES: usize = 64;
/// ...or this many bytes.
pub const PENDING_BYTES: i64 = 4 << 20;

/// Bumped when trigram extraction or the skip rules change: every file is reread and the lists rebuilt.
pub const CONTENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContentReport {
    /// Files discovery lists.
    pub files: usize,
    /// Files searched through the trigram lists.
    pub indexed: usize,
    /// Changed files searched directly until they are merged.
    pub pending: usize,
    /// Binary, large or generated files.
    pub skipped: usize,
    /// Files reread in this check.
    pub reread: usize,
    /// Files merged into the trigram lists in this check.
    pub merged: usize,
    pub rebuilt: bool,
    pub total_ms: f64,
}

/// Why a file is left out of search, if it is.
pub fn skip_reason(path: &str, bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() as u64 > max_search_bytes(path) {
        return Some("large");
    }
    if memchr::memchr(0, &bytes[..bytes.len().min(8000)]).is_some() {
        return Some("binary");
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".lock")
        || matches!(name, "Package.resolved" | "package-lock.json" | "pnpm-lock.yaml" | "go.sum")
        || name.contains(".min.")
    {
        return Some("generated");
    }
    let head = &bytes[..bytes.len().min(1024)];
    if memchr::memmem::find(head, b"@generated").is_some() || memchr::memmem::find(head, b"DO NOT EDIT").is_some() {
        return Some("generated");
    }
    // Minified or packed data: long files with very long lines.
    if bytes.len() > 4096 {
        let lines = memchr::memchr_iter(b'\n', bytes).count() + 1;
        if bytes.len() / lines > 500 {
            return Some("generated");
        }
    }
    None
}

/// Folds a byte for trigram purposes: ASCII letters are lowercased, everything else is kept.
#[inline]
pub fn fold(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

/// Packs three folded bytes into a trigram key.
#[inline]
pub fn gram(a: u8, b: u8, c: u8) -> u32 {
    (fold(a) as u32) << 16 | (fold(b) as u32) << 8 | fold(c) as u32
}

/// Collects the distinct trigrams of a buffer, ascending. A 2 MB bitmap per extractor keeps it linear; only the
/// words touched are visited and cleared.
pub struct GramSet {
    bits: Vec<u64>,
    touched: Vec<u32>,
}

impl Default for GramSet {
    fn default() -> Self {
        GramSet { bits: vec![0; 1 << 18], touched: Vec::new() }
    }
}

impl GramSet {
    pub fn grams(&mut self, bytes: &[u8]) -> Vec<u32> {
        if bytes.len() < 3 {
            return Vec::new();
        }
        let mut g = (fold(bytes[0]) as u32) << 8 | fold(bytes[1]) as u32;
        for &b in &bytes[2..] {
            g = ((g << 8) | fold(b) as u32) & 0xff_ffff;
            let w = (g >> 6) as usize;
            if self.bits[w] == 0 {
                self.touched.push(w as u32);
            }
            self.bits[w] |= 1 << (g & 63);
        }
        self.touched.sort_unstable();
        let mut out = Vec::with_capacity(self.touched.len() * 2);
        for &w in &self.touched {
            let mut word = std::mem::take(&mut self.bits[w as usize]);
            while word != 0 {
                let bit = word.trailing_zeros();
                out.push(w << 6 | bit);
                word &= word - 1;
            }
        }
        self.touched.clear();
        out
    }
}

/// What reading one changed file produced.
struct Read {
    idx: usize,
    mtime_ns: i64,
    size: i64,
    hash: String,
    skip: Option<&'static str>,
}

/// Pairs per chunk: 2 MB.
const CHUNK: usize = 1 << 18;

/// A chunk of pairs, at most [`CHUNK`], in memory of its own on Unix: mapped from the OS when made and given back when
/// dropped. The pairs are a cold index's largest allocation (87 MiB on bitwarden/clients); from the allocator, their
/// pages stayed counted against the process once freed, and the file ranking built next took as much again beside
/// them.
struct Chunk {
    #[cfg(unix)]
    ptr: std::ptr::NonNull<u64>,
    #[cfg(unix)]
    len: usize,
    #[cfg(not(unix))]
    vec: Vec<u64>,
}

// SAFETY: a chunk owns its mapping alone, like a `Vec<u64>`.
#[cfg(unix)]
unsafe impl Send for Chunk {}

#[cfg(unix)]
impl Chunk {
    const BYTES: usize = CHUNK * std::mem::size_of::<u64>();

    fn new() -> Chunk {
        // SAFETY: an anonymous private mapping, owned by this chunk until it is dropped. Pages are zeroed and touched
        // only as pairs are written.
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), Self::BYTES, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0)
        };
        match std::ptr::NonNull::new(p.cast::<u64>()) {
            Some(ptr) if p != libc::MAP_FAILED => Chunk { ptr, len: 0 },
            _ => std::alloc::handle_alloc_error(std::alloc::Layout::new::<[u64; CHUNK]>()),
        }
    }

    fn push(&mut self, pair: u64) {
        assert!(self.len < CHUNK);
        // SAFETY: within the mapping, which holds CHUNK pairs.
        unsafe { self.ptr.as_ptr().add(self.len).write(pair) };
        self.len += 1;
    }
}

#[cfg(unix)]
impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: the mapping made in `new`, unmapped once.
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), Self::BYTES) };
    }
}

#[cfg(unix)]
impl std::ops::Deref for Chunk {
    type Target = [u64];
    fn deref(&self) -> &[u64] {
        // SAFETY: the first `len` pairs are written.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(unix)]
impl std::ops::DerefMut for Chunk {
    fn deref_mut(&mut self) -> &mut [u64] {
        // SAFETY: as for `deref`, and the chunk is borrowed mutably.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(not(unix))]
impl Chunk {
    fn new() -> Chunk {
        Chunk { vec: Vec::with_capacity(CHUNK) }
    }

    fn push(&mut self, pair: u64) {
        self.vec.push(pair);
    }
}

#[cfg(not(unix))]
impl std::ops::Deref for Chunk {
    type Target = [u64];
    fn deref(&self) -> &[u64] {
        &self.vec
    }
}

#[cfg(not(unix))]
impl std::ops::DerefMut for Chunk {
    fn deref_mut(&mut self) -> &mut [u64] {
        &mut self.vec
    }
}

/// Trigram occurrences as `gram << 32 | file id`, which sort into trigram lists. They are held in fixed-size chunks,
/// each sorted once full, and merged when written: one growing array per thread, concatenated and sorted, needed up
/// to three times the pairs' size (spare capacity from doubling, then the copy).
#[derive(Default)]
struct Pairs {
    chunks: Vec<Chunk>,
    /// Indexes (into the file list) of the files whose trigrams are in `chunks`.
    files: Vec<usize>,
}

impl Pairs {
    fn push(&mut self, pair: u64) {
        match self.chunks.last_mut() {
            Some(c) if c.len() < CHUNK => c.push(pair),
            _ => {
                if let Some(full) = self.chunks.last_mut() {
                    full.sort_unstable();
                }
                let mut c = Chunk::new();
                c.push(pair);
                self.chunks.push(c);
            }
        }
    }

    /// Sorts the last, partly filled chunk. Call once all pairs are pushed.
    fn seal(&mut self) {
        if let Some(last) = self.chunks.last_mut() {
            last.sort_unstable();
        }
    }

    fn extend(&mut self, other: Pairs) {
        self.chunks.extend(other.chunks);
        self.files.extend(other.files);
    }

    /// Every pair, ascending: a merge of the sorted chunks.
    fn sorted(&self) -> impl Iterator<Item = u64> + '_ {
        use std::cmp::Reverse;
        let mut heap: std::collections::BinaryHeap<Reverse<(u64, usize, usize)>> =
            self.chunks.iter().enumerate().filter(|(_, c)| !c.is_empty()).map(|(k, c)| Reverse((c[0], k, 0))).collect();
        std::iter::from_fn(move || {
            let Reverse((pair, k, i)) = heap.pop()?;
            if let Some(&next) = self.chunks[k].get(i + 1) {
                heap.push(Reverse((next, k, i + 1)));
            }
            Some(pair)
        })
    }
}

/// Brings the content index for `root` up to date with the working tree. Run after [`crate::sync`], which keeps
/// the file list current.
pub fn sync_content(root: &Path, store: &mut Store, threads: usize) -> Result<ContentReport> {
    Ok(sync_content_files(root, store, threads)?.0)
}

/// [`sync_content`], also returning the files that exist on disk with their current state.
pub fn sync_content_files(root: &Path, store: &mut Store, threads: usize) -> Result<(ContentReport, Vec<CFile>)> {
    crate::writer::with_store(root, store, |store| sync_content_locked(root, store, threads))
}

fn sync_content_locked(root: &Path, store: &mut Store, threads: usize) -> Result<(ContentReport, Vec<CFile>)> {
    let t0 = Instant::now();
    let mut report = ContentReport::default();
    let version = CONTENT_VERSION.to_string();
    let stale_version = store.meta("content.version")?.as_deref() != Some(version.as_str());
    let mut files = store.cfiles()?;
    // Discovery never lists what the repo's AI privacy file blocks; this keeps a file from being read should the
    // file have changed since.
    let privacy = crate::privacy::Privacy::load(root);
    if privacy.blocks_anything() {
        files.retain(|f| !privacy.blocks(&f.path));
    }
    report.files = files.len();

    // Stat everything; reread what moved (or everything, after a version bump).
    let mark = crate::meter::Mark::now();
    let stats = stat_all(root, &files);
    let changed: Vec<usize> = (0..files.len())
        .filter(|&i| {
            let (f, (mtime_ns, size)) = (&files[i], stats[i]);
            // size -1: gone, a symlink or a submodule; discovery drops it later.
            // A file skipped as too large that a raised cap now admits (a ColdFusion page) is read again too.
            let admitted = f.skip.as_deref() == Some("large") && (size as u64) <= max_search_bytes(&f.path);
            size >= 0 && (stale_version || admitted || f.hash.is_empty() || f.mtime_ns != mtime_ns || f.size != size)
        })
        .collect();
    // On a first build every file is read once, trigrams included. Otherwise changed files wait as pending and
    // are read again only if they get merged.
    let first_build = stale_version || files.iter().all(|f| f.hash.is_empty());
    let (reads, mut pairs) = read_files(root, &files, &changed, &stats, threads, first_build);
    report.reread = reads.len();
    let mut read_cost = mark.cost();

    let merged_before: usize = store.meta("content.merged")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut state: Vec<i64> = files.iter().map(|f| f.state).collect();
    let batch = store.batch()?;
    for r in &reads {
        let f = &files[r.idx];
        let new_state = match r.skip {
            Some(_) => SKIPPED,
            None if !stale_version && f.state == INDEXED && f.hash == r.hash => INDEXED,
            None => PENDING,
        };
        state[r.idx] = new_state;
        batch.update_cfile(&CFileUpdate {
            id: f.id,
            mtime_ns: r.mtime_ns,
            size: r.size,
            hash: &r.hash,
            state: new_state,
            skip: r.skip,
        })?;
    }

    let live = |i: usize| stats[i].1 >= 0;
    let pending: Vec<usize> = (0..files.len()).filter(|&i| state[i] == PENDING && live(i)).collect();
    let pending_bytes: i64 = pending.iter().map(|&i| stats[i].1).sum();
    let indexed = (0..files.len()).filter(|&i| state[i] == INDEXED).count();
    if pending.len() > PENDING_FILES || pending_bytes > PENDING_BYTES || first_build {
        let rebuild = first_build || indexed == 0 || merged_before + pending.len() > (indexed / 4).max(256);
        let wanted: Vec<usize> = if rebuild {
            (0..files.len()).filter(|&i| matches!(state[i], INDEXED | PENDING) && live(i)).collect()
        } else {
            pending.clone()
        };
        // Read whatever we don't hold trigrams for yet.
        let held: std::collections::HashSet<usize> = pairs.files.iter().copied().collect();
        let missing: Vec<usize> = wanted.iter().copied().filter(|i| !held.contains(i)).collect();
        drop(held);
        let more_mark = crate::meter::Mark::now();
        let (more, more_pairs) = read_files(root, &files, &missing, &stats, threads, true);
        read_cost = read_cost + more_mark.cost();
        for r in &more {
            // Record what was read, so the stored hash always describes the indexed content.
            state[r.idx] = if r.skip.is_some() { SKIPPED } else { PENDING };
            batch.update_cfile(&CFileUpdate {
                id: files[r.idx].id,
                mtime_ns: r.mtime_ns,
                size: r.size,
                hash: &r.hash,
                state: state[r.idx],
                skip: r.skip,
            })?;
        }
        pairs.extend(more_pairs);
        if rebuild {
            batch.replace_grams(pairs.sorted())?;
            batch.set_meta("content.merged", "0")?;
        } else {
            batch.merge_grams(pairs.sorted())?;
            batch.set_meta("content.merged", &(merged_before + pairs.files.len()).to_string())?;
        }
        for &i in &pairs.files {
            if state[i] != INDEXED {
                state[i] = INDEXED;
                batch.set_cfile_state(files[i].id, INDEXED)?;
            }
        }
        report.merged = pairs.files.len();
        report.rebuilt = rebuild;
        batch.set_meta("content.version", &version)?;
    }
    drop(pairs);
    batch.commit()?;
    if report.rebuilt {
        store.checkpoint()?;
    }
    // Reading (stats, hashes, trigrams) on worker threads; the rest is writing.
    crate::meter::record("content.read", read_cost);
    crate::meter::record("content.write", mark.cost() - read_cost);

    for (i, st) in state.iter().enumerate() {
        match *st {
            INDEXED => report.indexed += 1,
            PENDING if live(i) => report.pending += 1,
            SKIPPED => report.skipped += 1,
            _ => {}
        }
    }
    report.total_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let current = files
        .into_iter()
        .enumerate()
        .filter(|&(i, _)| live(i))
        .map(|(i, mut f)| {
            f.state = state[i];
            f
        })
        .collect();
    Ok((report, current))
}

/// `(mtime_ns, size)` per file, with size -1 for anything that isn't a regular file. Stats run on a few threads:
/// a big repo has thousands of files, and this runs on every search.
fn stat_all(root: &Path, files: &[CFile]) -> Vec<(i64, i64)> {
    let one = |f: &CFile| {
        std::fs::symlink_metadata(root.join(&f.path))
            .ok()
            .filter(|m| m.is_file())
            .map_or((0, -1), |m| (crate::git::mtime_ns(&m), m.len() as i64))
    };
    if files.len() < 512 {
        return files.iter().map(one).collect();
    }
    let chunk = files.len().div_ceil(4);
    std::thread::scope(|scope| {
        let parts: Vec<_> = files.chunks(chunk).map(|c| scope.spawn(move || c.iter().map(one).collect::<Vec<_>>())).collect();
        parts.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

/// Size and content hash (as [`read_files`] records it) of a file larger than `cap`, read in blocks. `None` when it
/// turns out no larger than that.
fn hash_large(path: &Path, cap: u64) -> std::io::Result<Option<(i64, String)>> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut block = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = match file.read(&mut block) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&block[..n]);
        size += n as u64;
    }
    Ok((size > cap).then(|| (size as i64, hasher.finalize().to_hex()[..32].to_string())))
}

/// Reads files in parallel: hash, skip reason and, with `grams`, the trigrams of every searchable one.
fn read_files(
    root: &Path,
    files: &[CFile],
    which: &[usize],
    stats: &[(i64, i64)],
    threads: usize,
    grams: bool,
) -> (Vec<Read>, Pairs) {
    if which.is_empty() {
        return (Vec::new(), Pairs::default());
    }
    let next = AtomicUsize::new(0);
    let out = Mutex::new((Vec::with_capacity(which.len()), Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, which.len()) {
            scope.spawn(|| {
                // Pending-only reads and skipped files never need the 2 MB bitmap.
                let mut set = None::<GramSet>;
                let (mut local, mut pairs) = (Vec::new(), Pairs::default());
                let mut bytes = Vec::new();
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&idx) = which.get(k) else { break };
                    let f = &files[idx];
                    let path = root.join(&f.path);
                    // A file too large to search is only hashed, a block at a time: reading it whole would leave each
                    // thread's buffer as large as the largest file it met (archives, databases, tens of MB).
                    let cap = max_search_bytes(&f.path);
                    if stats[idx].1 as u64 > cap {
                        match hash_large(&path, cap) {
                            Ok(Some((size, hash))) => {
                                local.push(Read { idx, mtime_ns: stats[idx].0, size, hash, skip: Some("large") });
                                continue;
                            }
                            Ok(None) => {} // shrank since the stat: read it as usual
                            Err(_) => continue,
                        }
                    }
                    if crate::search::read_into(&path, &mut bytes).is_err() {
                        continue;
                    }
                    let skip = skip_reason(&f.path, &bytes);
                    if grams && skip.is_none() {
                        let id = f.id as u64;
                        for g in set.get_or_insert_with(GramSet::default).grams(&bytes) {
                            pairs.push((g as u64) << 32 | id);
                        }
                        pairs.files.push(idx);
                    }
                    local.push(Read {
                        idx,
                        mtime_ns: stats[idx].0,
                        size: bytes.len() as i64,
                        hash: blake3::hash(&bytes).to_hex()[..32].to_string(),
                        skip,
                    });
                }
                pairs.seal();
                let mut o = out.lock().unwrap();
                o.0.extend(local);
                o.1.push(pairs);
            });
        }
    });
    let (mut reads, parts) = out.into_inner().unwrap();
    reads.sort_by_key(|r| r.idx);
    let mut all = Pairs::default();
    for p in parts {
        all.extend(p);
    }
    all.files.sort_unstable();
    (reads, all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grams_are_folded_and_distinct() {
        let mut set = GramSet::default();
        let g = set.grams(b"AbcABC");
        let want: Vec<u32> = {
            let mut v = vec![gram(b'a', b'b', b'c'), gram(b'b', b'c', b'a'), gram(b'c', b'a', b'b')];
            v.sort();
            v
        };
        assert_eq!(g, want);
        // The bitmap is clean for the next file.
        assert_eq!(set.grams(b"xyz"), vec![gram(b'x', b'y', b'z')]);
        assert!(set.grams(b"ab").is_empty());
    }

    #[test]
    fn pairs_come_out_ascending_across_chunks() {
        // Two full chunks and part of a third, pushed out of order, from two threads' worth of pairs.
        let n = 2 * CHUNK + 5;
        let value = |i: usize| (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 8;
        let (mut a, mut b) = (Pairs::default(), Pairs::default());
        for i in 0..n {
            a.push(value(i));
        }
        b.push(7);
        b.push(3);
        a.seal();
        b.seal();
        a.extend(b);
        assert_eq!(a.chunks.len(), 4);
        assert!(a.chunks.iter().all(|c| c.len() <= CHUNK && c.windows(2).all(|w| w[0] <= w[1])));
        let mut want: Vec<u64> = (0..n).map(value).chain([7, 3]).collect();
        want.sort_unstable();
        assert_eq!(a.sorted().collect::<Vec<_>>(), want);
    }

    #[test]
    fn skips_binary_and_generated() {
        assert_eq!(skip_reason("a.png", b"\x89PNG\0\0"), Some("binary"));
        assert_eq!(skip_reason("Cargo.lock", b"[[package]]"), Some("generated"));
        assert_eq!(skip_reason("x.pb.rs", b"// @generated by prost\nfn x() {}"), Some("generated"));
        assert_eq!(skip_reason("app.min.js", b"var a"), Some("generated"));
        assert_eq!(skip_reason("long.js", &[b'a'; 5000]), Some("generated"));
        assert_eq!(skip_reason("a.swift", b"struct A {}\n"), None);
        // A ColdFusion page is searched up to its larger extraction cap.
        let big = "<cfset x = 1>\n".repeat(110_000);
        assert_eq!(skip_reason("page.cfm", big.as_bytes()), None);
        assert_eq!(skip_reason("Page.cs", big.as_bytes()), Some("large"));
    }
}
