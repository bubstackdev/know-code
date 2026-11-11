//! Presentation equivalence, derived only from explicit compiler evidence.
use crate::{
    Error, Result,
    query::{QueryRequest, ReadView},
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

pub(crate) const POLICY_VERSION: u32 = 1;
fn field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}
pub(crate) struct StorageIdentity<'a> {
    pub family: &'a str,
    pub declaration_key: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub signature: &'a str,
    pub external: bool,
    pub test_mask: u8,
    pub declarations: Option<[u8; 32]>,
}
pub(crate) fn storage_key(p: StorageIdentity<'_>) -> Option<String> {
    if p.external || p.family.is_empty() || p.declaration_key.is_empty() {
        return None;
    }
    let declarations = p.declarations?;
    let mut h = Sha256::new();
    field(&mut h, b"know-code-target-group-v1");
    for v in [p.family, p.declaration_key, p.name, p.kind, p.signature] {
        field(&mut h, v.as_bytes());
    }
    h.update([p.test_mask]);
    h.update(declarations);
    Some(format!("g:{:x}", h.finalize()))
}
pub(crate) struct DeclarationDigest {
    hash: Sha256,
    count: u64,
    proven: bool,
}
impl Default for DeclarationDigest {
    fn default() -> Self {
        let mut hash = Sha256::new();
        field(&mut hash, b"know-code-target-declarations-v1");
        Self {
            hash,
            count: 0,
            proven: true,
        }
    }
}
impl DeclarationDigest {
    pub(crate) fn push(
        &mut self,
        artifact_id: &str,
        path: &str,
        content_hash: &str,
        start: u64,
        end: u64,
        generated: bool,
    ) {
        // Generated display paths are not generator/hint evidence. They cannot
        // establish equivalence until the adapter can provide that evidence.
        self.proven &= !generated;
        for v in [artifact_id, path, content_hash] {
            field(&mut self.hash, v.as_bytes());
        }
        self.hash.update(start.to_le_bytes());
        self.hash.update(end.to_le_bytes());
        self.hash.update([u8::from(generated)]);
        self.count += 1;
    }
    pub(crate) fn finish(self) -> Option<[u8; 32]> {
        (self.proven && self.count > 0).then(|| self.hash.finalize().into())
    }
}
pub(crate) fn create_view(c: &Connection, version: i32) -> Result<()> {
    let key = if version >= 8 {
        "CASE WHEN grouping_key<>'' THEN grouping_key ELSE 's:'||rowid END"
    } else {
        "'s:'||rowid"
    };
    let mask = if version >= 7 { "" } else { ",1 AS test_mask" };
    c.execute_batch(&format!("CREATE TEMP VIEW grouped_symbols AS SELECT *,rowid AS symbol_key,{key} AS finding_key{mask} FROM symbols;"))?;
    Ok(())
}
pub(crate) fn finalize(c: &Connection, generation: u64, compilation: &str) -> Result<()> {
    let mut q=c.prepare("SELECT s.rowid,c.target_family,s.declaration_key,s.name,s.kind,s.signature,s.external,s.test_mask,a.id,a.path,a.content_hash,o.start_byte,o.end_byte,a.generated,EXISTS(SELECT 1 FROM unit_artifacts ua WHERE ua.owner_generation=s.generation AND ua.compilation_id=s.compilation_id AND ua.artifact_id=a.id) FROM symbols s JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id LEFT JOIN occurrences o ON o.symbol_key=s.rowid AND o.role='declaration' LEFT JOIN artifacts a ON a.rowid=o.artifact_key WHERE s.generation=? AND s.compilation_id=? AND c.target_equivalence_policy=1 AND s.declaration_key<>'' AND s.external=0 ORDER BY s.rowid,a.path,a.content_hash,a.id,o.start_byte,o.end_byte,a.generated")?;
    let mut rows = q.query(params![generation, compilation])?;
    let mut current = None;
    let mut identity = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        false,
        1u8,
    );
    let mut digest = DeclarationDigest::default();
    let save = |key: i64,
                p: &(String, String, String, String, String, bool, u8),
                d: DeclarationDigest|
     -> Result<()> {
        if let Some(value) = storage_key(StorageIdentity {
            family: &p.0,
            declaration_key: &p.1,
            name: &p.2,
            kind: &p.3,
            signature: &p.4,
            external: p.5,
            test_mask: p.6,
            declarations: d.finish(),
        }) {
            c.execute(
                "UPDATE symbols SET grouping_key=? WHERE rowid=?",
                params![value, key],
            )?;
        }
        Ok(())
    };
    while let Some(r) = rows.next()? {
        let key = r.get::<_, i64>(0)?;
        if current != Some(key) {
            if let Some(old) = current {
                save(old, &identity, std::mem::take(&mut digest))?;
            }
            current = Some(key);
            identity = (
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            );
        }
        if let Some(id) = r.get::<_, Option<String>>(8)? {
            digest.proven &= r.get::<_, bool>(14)?;
            digest.push(
                &id,
                &r.get::<_, String>(9)?,
                &r.get::<_, String>(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
            );
        }
    }
    if let Some(key) = current {
        save(key, &identity, digest)?;
    }
    Ok(())
}
// Private singleton keys address one row; proven keys seek the sparse
// grouping index. Never interpret an opaque public symbol ID as a key.
pub(crate) fn members(
    view: &ReadView,
    request: &QueryRequest,
    group: &str,
    mut visit: impl FnMut(i64) -> Result<()>,
) -> Result<()> {
    members_until(view, request, group, |key| {
        visit(key)?;
        Ok(true)
    })?;
    Ok(())
}
fn members_until(
    view: &ReadView,
    request: &QueryRequest,
    group: &str,
    mut visit: impl FnMut(i64) -> Result<bool>,
) -> Result<bool> {
    let singleton = group.strip_prefix("s:").and_then(|s| s.parse::<i64>().ok());
    let predicate = if singleton.is_some() {
        "s.rowid=?2"
    } else {
        "s.grouping_key=?2 AND s.grouping_key<>''"
    };
    let mut q=view.connection.prepare_cached(&format!("SELECT s.rowid FROM symbols s CROSS JOIN generation_units u ON u.owner_generation=s.generation AND u.compilation_id=s.compilation_id CROSS JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id WHERE u.generation=?1 AND {predicate} AND (?3 IS NULL OR c.target=?3) AND (?4 IS NULL OR s.compilation_id=?4) ORDER BY c.target,s.compilation_id,s.id"))?;
    let lookup: &dyn rusqlite::ToSql = if let Some(ref row) = singleton {
        row
    } else {
        &group
    };
    let mut rows = q.query(params![
        view.generation,
        lookup,
        request.target,
        request.compilation_id
    ])?;
    while let Some(r) = rows.next()? {
        view.check()?;
        if !visit(r.get(0)?)? {
            return Ok(false);
        }
    }
    Ok(true)
}
// Resolve the original member first. Filters may narrow its peers, but cannot
// make an absent original valid, nor use tests to erase a traversal root.
pub(crate) fn roots(view: &ReadView, request: &QueryRequest) -> Result<Vec<i64>> {
    Ok(roots_bounded(view, request, None)?.0)
}
pub(crate) fn roots_bounded(
    view: &ReadView,
    request: &QueryRequest,
    maximum: Option<usize>,
) -> Result<(Vec<i64>, bool)> {
    let id = request
        .symbol_id
        .as_ref()
        .ok_or_else(|| Error::InvalidRequest("symbol_id is required".into()))?;
    let mut q=view.connection.prepare_cached("SELECT DISTINCT s.finding_key FROM generation_units u CROSS JOIN grouped_symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id WHERE u.generation=? AND s.id=? ORDER BY s.finding_key")?;
    let mut rows = q.query(params![view.generation, id])?;
    let mut result = vec![];
    let mut limited = false;
    while let Some(r) = rows.next()? {
        let group = r.get::<_, String>(0)?;
        if !members_until(view, request, &group, |key| {
            if maximum.is_some_and(|max| result.len() >= max) {
                limited = true;
                return Ok(false);
            }
            result.push(key);
            Ok(true)
        })? {
            break;
        }
    }
    if result.is_empty() {
        return Err(Error::InvalidRequest("symbol_id is absent from the served generation or has no members in the requested compilation/target scope".into()));
    }
    Ok((result, limited))
}
pub(crate) fn key(c: &Connection, key: i64) -> Result<String> {
    Ok(c.query_row(
        "SELECT finding_key FROM grouped_symbols WHERE symbol_key=?",
        [key],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn digest(generated: bool, last: u64) -> Option<[u8; 32]> {
        let mut d = DeclarationDigest::default();
        for i in 0..20 {
            d.push(
                "a",
                "f.cs",
                "hash",
                i,
                if i == 19 { last } else { i + 1 },
                generated,
            );
        }
        d.finish()
    }
    fn key(d: Option<[u8; 32]>, mask: u8) -> Option<String> {
        storage_key(StorageIdentity {
            family: "family",
            declaration_key: "compiler",
            name: "F",
            kind: "method",
            signature: "void F()",
            external: false,
            test_mask: mask,
            declarations: d,
        })
    }
    #[test]
    fn full_anchors_and_masks_define_groups() {
        assert_eq!(key(digest(false, 20), 4), key(digest(false, 20), 4));
        assert_ne!(key(digest(false, 20), 4), key(digest(false, 21), 4));
        assert_ne!(key(digest(false, 20), 4), key(digest(false, 20), 1));
        assert!(key(digest(true, 20), 4).is_none());
        assert!(key(None, 4).is_none());
    }
    #[test]
    fn length_framing_is_unambiguous() {
        let mut a = DeclarationDigest::default();
        a.push("ab", "c", "d", 1, 2, false);
        let mut b = DeclarationDigest::default();
        b.push("a", "bc", "d", 1, 2, false);
        assert_ne!(a.finish(), b.finish());
    }
}
