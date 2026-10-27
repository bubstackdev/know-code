//! Text files as `locate` documents: the YAML, JSON, Markdown, shell, Dockerfile, Terraform, template (`.tftpl`,
//! `.tpl`, `.j2`) and unindexed ColdFusion files discovery lists but the index holds no symbols for (see
//! [`kc_store::locate::text_document`]), and templates the index does hold (an Ansible role's `.j2`), whose text
//! joins the words of their indexed row.
//!
//! Documents are cached in the index with each file's mtime and size; a query stats the candidates and rereads only
//! the files that changed, so a config edit shows in the next `locate`.

use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use kc_store::Store;
use kc_store::locate::{TEXT_READ_CAP, TextDoc, text_document, text_kind};
use serde::{Deserialize, Serialize};

/// Bumped when the documents' shape or the rules reading them change.
const TEXT_DOCS_VERSION: u32 = 2;
/// A JSON file larger than this is data, not configuration.
const MAX_JSON_BYTES: u64 = 64 << 10;
/// Text files larger than this are left out.
const MAX_TEXT_BYTES: u64 = 1 << 20;

#[derive(Serialize, Deserialize)]
struct Cached {
    mtime_ns: i64,
    size: u64,
    doc: TextDoc,
}

/// The documents of `root`'s text files (see the module docs), in path order.
pub fn docs(root: &Path, store: &Store) -> Result<Vec<TextDoc>> {
    let conn = store.conn();
    let mut stmt = conn.prepare_cached(
        "SELECT c.path, EXISTS (SELECT 1 FROM files f WHERE f.path = c.path) FROM cfiles c WHERE c.skip IS NULL ORDER BY c.path",
    )?;
    let paths: Vec<(String, bool)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut listed: Vec<(String, &'static str, i64, u64)> = Vec::new();
    for (p, indexed) in paths {
        let Some(kind) = text_kind(&p) else { continue };
        // An indexed file's own facts make its document; a template (an Ansible `.j2`) adds its text to them. A config
        // file read only for its hosts has no facts a document is made of: its text is its document.
        if indexed && kind != "template" && !kc_extract::hosts::is_host_config(&p) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(root.join(&p)) else { continue };
        let max = if kind == "json" { MAX_JSON_BYTES } else { MAX_TEXT_BYTES };
        if !meta.is_file() || meta.len() > max {
            continue;
        }
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
        listed.push((p, kind, mtime, meta.len()));
    }
    let mut h = blake3::Hasher::new();
    for (p, _, m, s) in &listed {
        h.update(format!("{p}\0{m}\0{s}\0").as_bytes());
    }
    let generation = h.finalize().to_hex()[..16].to_string();
    let key = format!("locate_texts:{TEXT_DOCS_VERSION}");
    let old: HashMap<String, Cached> = match store.derived_latest::<Vec<Cached>>(&key)? {
        Some((g, list)) if g == generation => return Ok(list.into_iter().map(|c| c.doc).collect()),
        Some((_, list)) => list.into_iter().map(|c| (c.doc.path.clone(), c)).collect(),
        None => HashMap::new(),
    };
    let mut old = old;
    let list = store.derived(&key, &generation, || {
        let mut out = Vec::with_capacity(listed.len());
        for (p, kind, mtime_ns, size) in &listed {
            if let Some(c) = old.remove(p).filter(|c| c.mtime_ns == *mtime_ns && c.size == *size) {
                out.push(c);
                continue;
            }
            let Ok(bytes) = std::fs::read(root.join(p)) else { continue };
            let head = &bytes[..bytes.len().min(TEXT_READ_CAP)];
            if memchr::memchr(0, head).is_some() {
                continue;
            }
            let text = kc_extract::decode(head);
            out.push(Cached { mtime_ns: *mtime_ns, size: *size, doc: text_document(p, kind, &text) });
        }
        Ok(out)
    })?;
    Ok(list.into_iter().map(|c| c.doc).collect())
}
