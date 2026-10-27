//! A declaration's identity beyond its qualified name: the namespace or module it is declared in.
//!
//! A qualified name (`Worker.Run`) is the containing types and the name. Two declarations in different namespaces
//! (C# `A.Worker.Run` and `B.Worker.Run`) or modules (`src/a.ts` and `src/b.ts`) share it, so a selector can name the
//! namespace or module too, and answers listing several such declarations say which is which (`namespace`).
//!
//! The namespace is the C# namespace of the declaration or its nearest containing type. Other languages have no
//! namespace in the index; their module stands in, from the file's path: TypeScript and JavaScript the path without
//! its extension (`src/worker`, an `index` file its directory), Python the dotted module (`pkg.worker`, an
//! `__init__.py` its package), Swift the target of a SwiftPM layout (`Sources/<Target>/…`). Others have none.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::query::SymbolRow;

/// The namespace or module `s` is declared in (see the module docs), `None` when there is none to tell.
pub fn namespace_of(conn: &Connection, s: &SymbolRow) -> Result<Option<String>> {
    let mut id = Some(s.id);
    // Nesting is shallow; the bound guards against a malformed parent chain.
    for _ in 0..32 {
        let Some(i) = id else { break };
        let row: Option<(Option<String>, Option<i64>)> =
            conn.prepare_cached("SELECT type_name, parent_id FROM symbols WHERE id = ?1")?.query_row([i], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let Some((type_name, parent)) = row else { break };
        if let Some(ns) = kc_extract::TypeDecl::decode(type_name.as_deref()).ns.filter(|n| !n.is_empty()) {
            return Ok(Some(ns));
        }
        id = parent;
    }
    let lang: Option<String> = conn.prepare_cached("SELECT lang FROM files WHERE path = ?1")?.query_row([&s.file], |r| r.get(0)).optional()?;
    Ok(lang.and_then(|l| module_of(&s.file, &l)))
}

/// The module a file of language `lang` is (see the module docs).
pub fn module_of(path: &str, lang: &str) -> Option<String> {
    let stem = |p: &str| p.rsplit_once('.').filter(|(head, _)| !head.ends_with('/') && !head.is_empty()).map_or(p.to_string(), |(h, _)| h.to_string());
    match lang {
        "typescript" | "tsx" | "javascript" => {
            let s = stem(path);
            // `foo.d.ts`, `foo.component.ts`: everything before the language's own extension stays.
            let s = s.strip_suffix(".d").map_or(s.clone(), str::to_string);
            Some(s.strip_suffix("/index").map_or(s.clone(), str::to_string))
        }
        "python" => {
            let s = stem(path);
            let s = s.strip_suffix("/__init__").map_or(s.clone(), str::to_string);
            Some(s.replace('/', "."))
        }
        "swift" => {
            let mut parts = path.split('/');
            while let Some(p) = parts.next() {
                if p == "Sources" {
                    let target = parts.next()?;
                    return parts.next().map(|_| target.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// `src/a.b` and `src::a::b` as `src.a.b`.
fn dotted(s: &str) -> String {
    s.replace("::", ".").replace('/', ".")
}

/// Whether a selector's qualifier names the namespace or module `ns`: the whole of it, or its last components
/// (`Orders.Api` names `Acme.Orders.Api`; `worker` names `src/worker`).
pub fn qualifies(ns: &str, qualifier: &str) -> bool {
    let (ns, q) = (dotted(ns), dotted(qualifier.trim_matches('.')));
    !q.is_empty() && (ns == q || ns.ends_with(&format!(".{q}")))
}

/// The definitions a namespace- or module-qualified selector names (`B.Worker.Run`, `src/worker.Worker.run`,
/// `pkg.mod.Worker.run`): for each way of splitting it at a dot into a qualifier and a rest, the definitions `lookup`
/// finds for the rest whose namespace the qualifier names. Empty when the query has no such split.
pub fn namespaced(
    conn: &Connection,
    query: &str,
    mut lookup: impl FnMut(&str) -> Result<Vec<SymbolRow>>,
) -> Result<Vec<SymbolRow>> {
    let query = query.trim();
    let mut out: Vec<SymbolRow> = Vec::new();
    for (i, _) in query.match_indices('.') {
        let (qualifier, rest) = (&query[..i], &query[i + 1..]);
        if qualifier.is_empty() || rest.is_empty() {
            continue;
        }
        for s in lookup(rest)? {
            if out.iter().any(|o| o.id == s.id) {
                continue;
            }
            if let Some(ns) = namespace_of(conn, &s)?
                && qualifies(&ns, qualifier)
            {
                out.push(SymbolRow { namespace: Some(ns), ..s });
            }
        }
    }
    Ok(out)
}

/// Sets `namespace` on each of `rows` whose qualified name another of them shares, so an answer listing them tells
/// them apart.
pub fn disambiguate(conn: &Connection, rows: &mut [SymbolRow]) -> Result<()> {
    if rows.len() < 2 {
        return Ok(());
    }
    let names: Vec<String> = rows.iter().map(SymbolRow::qualified).collect();
    for i in 0..rows.len() {
        if rows[i].namespace.is_none() && names.iter().enumerate().any(|(j, n)| j != i && *n == names[i]) {
            rows[i].namespace = namespace_of(conn, &rows[i])?;
        }
    }
    Ok(())
}

/// Of `decls`, declarations sharing one qualified name (a module's `Grid` and another's `private struct Grid`), the
/// ids of those an extension declared in `ext` extends: the one in its own file, else its module's (a module's own
/// type shadows an imported one), else those of other modules, narrowed to the modules its file imports when it
/// imports any of them. A `private` or `fileprivate` declaration is seen from its file only. Files whose module can't
/// be told count as one module. Several when nothing tells them apart (two imported modules each declaring it): the
/// extension is ambiguous between them. Empty when it sees none of them: it extends a type the repo doesn't declare.
pub fn extended_by(conn: &Connection, ext: &SymbolRow, decls: &[SymbolRow]) -> Result<Vec<i64>> {
    let here = namespace_of(conn, ext)?;
    let mut ranked = Vec::with_capacity(decls.len());
    for d in decls {
        let ns = namespace_of(conn, d)?;
        let rank = if d.file == ext.file {
            0
        } else if file_private(&d.signature) {
            continue;
        } else if ns == here {
            1
        } else {
            2
        };
        ranked.push((rank, d.id, ns));
    }
    let Some(best) = ranked.iter().map(|r| r.0).min() else { return Ok(Vec::new()) };
    ranked.retain(|r| r.0 == best);
    if best == 2 {
        let imports: Vec<String> = conn
            .prepare_cached("SELECT i.module FROM imports i JOIN files f ON f.id = i.file_id WHERE f.path = ?1")?
            .query_map([&ext.file], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let imported = |ns: &Option<String>| ns.as_deref().is_some_and(|n| imports.iter().any(|i| crate::query::imports_module(i, n)));
        if ranked.iter().any(|r| imported(&r.2)) {
            ranked.retain(|r| imported(&r.2));
        }
    }
    Ok(ranked.into_iter().map(|r| r.1).collect())
}

/// Whether a Swift declaration is seen from its own file only: written `private` or `fileprivate`.
fn file_private(signature: &str) -> bool {
    let head = signature.split(['(', '<', ':', '{', '=']).next().unwrap_or("");
    head.split_whitespace().any(|w| w == "private" || w == "fileprivate")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_come_from_paths() {
        assert_eq!(module_of("src/worker.ts", "typescript").as_deref(), Some("src/worker"));
        assert_eq!(module_of("src/ui/index.tsx", "tsx").as_deref(), Some("src/ui"));
        assert_eq!(module_of("src/app.component.ts", "typescript").as_deref(), Some("src/app.component"));
        assert_eq!(module_of("types/api.d.ts", "typescript").as_deref(), Some("types/api"));
        assert_eq!(module_of("pkg/worker.py", "python").as_deref(), Some("pkg.worker"));
        assert_eq!(module_of("pkg/__init__.py", "python").as_deref(), Some("pkg"));
        assert_eq!(module_of("Sources/Net/Client.swift", "swift").as_deref(), Some("Net"));
        assert_eq!(module_of("App/Client.swift", "swift"), None);
        assert_eq!(module_of("a.cs", "csharp"), None);
    }

    #[test]
    fn qualifiers_name_a_namespace_whole_or_by_its_end() {
        assert!(qualifies("B", "B"));
        assert!(qualifies("Acme.Orders.Api", "Orders.Api"));
        assert!(!qualifies("Acme.Orders.Api", "rders.Api"));
        assert!(qualifies("src/worker", "worker") && qualifies("src/worker", "src/worker") && qualifies("src/worker", "src.worker"));
        assert!(!qualifies("A", "B") && !qualifies("A", ""));
    }
}
