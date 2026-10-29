//! Java name resolution: a name binds where Java's own rules put it, or stays unknown.
//!
//! A simple type name is looked up as the compiler does (JLS 6.4.1, 7.5): the types the code is nested in and their
//! member types, then single-type imports, then the file's own package, then on-demand imports (`a.b.*`). A name a
//! single-type import brings from outside the repository, or one none of these reaches (a `java.lang` type, say),
//! is outside: it is never linked to a same-named declaration of the repository's. A static member named through a
//! type outside the repository (`Collections.emptyList()`, `BigInteger.ONE`) is outside the same way, and so is a
//! bare call that a static import brings from outside (`assertThat` from `import static …Truth.assertThat`), unless
//! the caller's own types declare it.
//!
//! What these rules can't settle (a member of an expression whose type isn't written) goes on to the shared
//! resolution in `query`, and [`Resolver::after`] keeps its answer from naming one declaration by name alone.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::query::RefHit;

/// Whether a reference in this file resolves by Java's rules.
pub fn handles(file: &str) -> bool {
    file.ends_with(".java")
}

/// A type the repository declares in Java.
#[derive(Debug, Clone)]
struct JavaType {
    id: i64,
    /// A local class's (one declared in a method body): its file and the method's lines, where alone it is seen.
    local: Option<(String, u32, u32)>,
    /// The dotted names of the types it is nested in (`Outer.Inner` for a type in `Inner`).
    container: Option<String>,
    package: Option<String>,
}

impl JavaType {
    /// `package.Outer.Name`, or `Outer.Name` in the default package.
    fn qualified(&self, name: &str) -> String {
        let mut q = String::new();
        if let Some(p) = &self.package {
            q.push_str(p);
            q.push('.');
        }
        if let Some(c) = &self.container {
            q.push_str(c);
            q.push('.');
        }
        q.push_str(name);
        q
    }
}

/// What a type name written in a file is.
#[derive(Debug, Clone, PartialEq)]
enum Found {
    /// Repository types the rules reach (several when on-demand imports reach more than one).
    Inside(Vec<i64>),
    /// A type from outside the repository: a single-type import of another package's, or a name no rule reaches and
    /// the repository doesn't declare where the file can see it.
    Outside,
    /// Repository types of that name exist, but none the rules reach (a member type inherited from a supertype is
    /// the likely case): candidates, not a link.
    Unreached(Vec<i64>),
}

#[derive(Default)]
struct FileScope {
    package: Option<String>,
    /// `a.b.C` imports: simple name to qualified name.
    single: HashMap<String, String>,
    /// `a.b.*` imports: the package or type named.
    on_demand: Vec<String>,
    /// `static a.b.C.m` imports: member name to the type's qualified name.
    static_single: HashMap<String, Vec<String>>,
    /// `static a.b.C.*` imports: the types named.
    static_on_demand: Vec<String>,
}

/// Java's types, loaded once per resolution, and each file's package and imports, as asked for.
#[derive(Default)]
pub struct Resolver {
    loaded: bool,
    /// By simple name.
    types: HashMap<String, Vec<(String, JavaType)>>,
    /// By id: the type's simple name.
    names: HashMap<i64, String>,
    /// By qualified name.
    qualified: HashMap<String, i64>,
    /// Packages the repository declares types in.
    packages: HashSet<String>,
    files: HashMap<String, FileScope>,
    /// Per type id: the simple names of its direct supertypes, as written.
    supers: HashMap<i64, Vec<String>>,
}

impl Resolver {
    fn load(&mut self, conn: &Connection) -> rusqlite::Result<()> {
        if self.loaded {
            return Ok(());
        }
        self.loaded = true;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, s.container,
                    CASE WHEN json_valid(s.type_name) THEN json_extract(s.type_name, '$.ns') END,
                    CASE WHEN p.kind IN ('method', 'init', 'property', 'case') THEN f.path END, p.start_line, p.end_line
             FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
             WHERE f.lang = 'java' AND s.kind IN ('class', 'protocol', 'enum', 'struct', 'typealias')",
        )?;
        let rows = stmt.query_map([], |r| {
            let local = match r.get::<_, Option<String>>(4)? {
                Some(file) => Some((file, r.get::<_, u32>(5)?, r.get::<_, u32>(6)?)),
                None => None,
            };
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, local))
        })?;
        for row in rows {
            let (id, name, container, package, local) = row?;
            let t = JavaType { id, local, container, package };
            if let Some(p) = &t.package {
                self.packages.insert(p.clone());
            }
            if t.local.is_none() {
                self.qualified.insert(t.qualified(&name), id);
            }
            self.names.insert(id, name.clone());
            self.types.entry(name.clone()).or_default().push((name, t));
        }
        let mut stmt = conn.prepare(
            "SELECT r.from_id, r.target_name FROM relations r JOIN files f ON f.id = r.file_id
             WHERE f.lang = 'java' AND r.kind = 'inherits'",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (from, target) = row?;
            self.supers.entry(from).or_default().push(target);
        }
        Ok(())
    }

    fn scope(&mut self, conn: &Connection, file: &str) -> rusqlite::Result<&FileScope> {
        if !self.files.contains_key(file) {
            let package = conn
                .query_row(
                    "SELECT json_extract(s.type_name, '$.ns') FROM symbols s JOIN files f ON f.id = s.file_id
                     WHERE f.path = ?1 AND s.parent_id IS NULL AND json_valid(s.type_name)
                       AND json_extract(s.type_name, '$.ns') IS NOT NULL LIMIT 1",
                    params![file],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let mut s = FileScope { package, ..Default::default() };
            let mut stmt = conn.prepare("SELECT i.module FROM imports i JOIN files f ON f.id = i.file_id WHERE f.path = ?1")?;
            for m in stmt.query_map(params![file], |r| r.get::<_, String>(0))? {
                let m = m?;
                let (is_static, q) = match m.strip_prefix("static ") {
                    Some(q) => (true, q.to_string()),
                    None => (false, m.clone()),
                };
                match (is_static, q.strip_suffix(".*")) {
                    (false, Some(p)) => s.on_demand.push(p.to_string()),
                    (false, None) => {
                        let simple = q.rsplit('.').next().unwrap_or(&q).to_string();
                        s.single.insert(simple, q);
                    }
                    (true, Some(t)) => s.static_on_demand.push(t.to_string()),
                    (true, None) => {
                        if let Some((t, member)) = q.rsplit_once('.') {
                            s.static_single.entry(member.to_string()).or_default().push(t.to_string());
                        }
                    }
                }
            }
            self.files.insert(file.to_string(), s);
        }
        Ok(&self.files[file])
    }

    /// The type a simple name written in `file`, in code nested in the types `nest` (innermost last, dotted names
    /// within the file's package), refers to.
    fn find_type(&self, file: &FileScope, at: (&str, u32), nest: &[String], name: &str) -> Found {
        let Some(all) = self.types.get(name) else {
            return Found::Outside;
        };
        // A local class, declared in a method enclosing the reference, comes first; one in another method is never
        // seen.
        let local: Vec<i64> = all
            .iter()
            .filter_map(|(_, t)| t.local.as_ref().filter(|(f, s, e)| f == at.0 && (*s..=*e).contains(&at.1)).map(|l| (t.id, l.1)))
            .max_by_key(|(_, start)| *start)
            .map(|(id, _)| id)
            .into_iter()
            .collect();
        if !local.is_empty() {
            return Found::Inside(local);
        }
        let all: Vec<&(String, JavaType)> = all.iter().filter(|(_, t)| t.local.is_none()).collect();
        if all.is_empty() {
            return Found::Outside;
        }
        let in_package = |t: &JavaType| t.package == file.package;
        // The types the code is nested in, and their member types, innermost first.
        for outer in nest.iter().rev() {
            if let Some((_, t)) = all.iter().find(|(_, t)| in_package(t) && t.container.as_deref() == Some(outer.as_str())) {
                return Found::Inside(vec![t.id]);
            }
            let (container, last) = match outer.rsplit_once('.') {
                Some((c, l)) => (Some(c), l),
                None => (None, outer.as_str()),
            };
            if last == name
                && let Some((_, t)) = all.iter().find(|(_, t)| in_package(t) && t.container.as_deref() == container)
            {
                return Found::Inside(vec![t.id]);
            }
        }
        if let Some(q) = file.single.get(name) {
            return self.qualified.get(q).map_or(Found::Outside, |id| Found::Inside(vec![*id]));
        }
        let top: Vec<i64> = all.iter().filter(|(_, t)| in_package(t) && t.container.is_none()).map(|(_, t)| t.id).collect();
        if !top.is_empty() {
            return Found::Inside(top);
        }
        let on_demand: Vec<i64> = all
            .iter()
            .filter(|(n, t)| {
                file.on_demand.iter().any(|p| {
                    t.package.as_deref() == Some(p.as_str()) && t.container.is_none() || self.qualified.get(&format!("{p}.{n}")) == Some(&t.id)
                })
            })
            .map(|(_, t)| t.id)
            .collect();
        if !on_demand.is_empty() {
            return Found::Inside(on_demand);
        }
        Found::Unreached(all.iter().map(|(_, t)| t.id).collect())
    }

    /// A qualified name's type: `a.b.C` (a package and a type), `Outer.Inner` (a type and its member type), or a
    /// mix. `None` when the qualifier isn't a type or package name at all (an expression).
    fn find_qualified(&self, file: &FileScope, at: (&str, u32), nest: &[String], qualifier: &str, name: &str) -> Option<Found> {
        let dotted = qualifier.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.');
        if !dotted || qualifier.is_empty() {
            return None;
        }
        let full = format!("{qualifier}.{name}");
        if let Some(id) = self.qualified.get(&full) {
            return Some(Found::Inside(vec![*id]));
        }
        // The qualifier's first segment: a type in scope, or a package.
        let (head, rest) = qualifier.split_once('.').map_or((qualifier, None), |(h, r)| (h, Some(r)));
        if head.chars().next().is_some_and(char::is_uppercase) {
            return Some(match self.find_type(file, at, nest, head) {
                Found::Inside(ids) => {
                    let found: Vec<i64> = ids
                        .iter()
                        .filter_map(|id| {
                            let outer = self.qualified.iter().find(|(_, v)| *v == id).map(|(k, _)| k.clone())?;
                            let path = rest.map_or_else(|| format!("{outer}.{name}"), |r| format!("{outer}.{r}.{name}"));
                            self.qualified.get(&path).copied()
                        })
                        .collect();
                    // A member type the repository doesn't declare on its type: inherited, or not a type at all.
                    if found.is_empty() { Found::Unreached(Vec::new()) } else { Found::Inside(found) }
                }
                Found::Outside => Found::Outside,
                Found::Unreached(_) => Found::Unreached(Vec::new()),
            });
        }
        // A package-qualified name: the repository's own packages, or another's.
        let package_like = qualifier.split('.').all(|s| s.chars().next().is_some_and(char::is_lowercase));
        if package_like {
            return Some(if self.packages.iter().any(|p| p == qualifier || p.starts_with(&format!("{qualifier}."))) {
                Found::Unreached(Vec::new())
            } else {
                Found::Outside
            });
        }
        None
    }

    /// The caller's types: the type it is in and the types around it, as dotted names within the package.
    fn nest(hit: &RefHit) -> Vec<String> {
        let Some(from) = &hit.from else { return Vec::new() };
        let inner = if matches!(from.kind.as_str(), "class" | "protocol" | "enum" | "struct") {
            Some(from.container.as_deref().map_or_else(|| from.name.clone(), |c| format!("{c}.{}", from.name)))
        } else {
            from.container.clone()
        };
        let Some(inner) = inner else { return Vec::new() };
        let parts: Vec<&str> = inner.split('.').collect();
        (1..=parts.len()).map(|n| parts[..n].join(".")).collect()
    }

    /// The names of the caller's types, their outer types and every repository supertype of those, for telling
    /// whether a bare name is a member the caller's own types declare or inherit.
    fn chain_names(&self, file: &FileScope, nest: &[String]) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut queue: Vec<i64> = nest
            .iter()
            .filter_map(|n| self.qualified.get(&file.package.as_ref().map_or_else(|| n.clone(), |p| format!("{p}.{n}"))).copied())
            .collect();
        let mut seen = HashSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(n) = self.names.get(&id) {
                out.insert(n.clone());
            }
            for s in self.supers.get(&id).into_iter().flatten() {
                if let Found::Inside(ids) = self.find_type(file, ("", 0), nest, s) {
                    queue.extend(ids);
                }
            }
        }
        out
    }

    /// Resolves a reference in a Java file when Java's rules decide it; `None` leaves it to the shared resolution.
    pub fn scoped(&mut self, conn: &Connection, hit: &RefHit) -> rusqlite::Result<Option<(&'static str, Vec<i64>)>> {
        self.load(conn)?;
        self.scope(conn, &hit.file)?;
        let file = &self.files[&hit.file];
        let nest = Self::nest(hit);
        let at = (hit.file.as_str(), hit.line);
        let typed = |found: Found| -> (&'static str, Vec<i64>) {
            match found {
                Found::Inside(ids) if ids.len() == 1 => ("exact", ids),
                Found::Inside(ids) => ("ambiguous", ids),
                Found::Outside => ("unresolved", Vec::new()),
                Found::Unreached(ids) if ids.is_empty() => ("unresolved", Vec::new()),
                Found::Unreached(ids) => ("ambiguous", ids),
            }
        };
        if matches!(hit.kind.as_str(), "type_ref" | "instantiate") {
            let found = match hit.receiver.as_deref() {
                None => self.find_type(file, at, &nest, &hit.name),
                Some(q) => match self.find_qualified(file, at, &nest, q, &hit.name) {
                    Some(f) => f,
                    None => return Ok(None),
                },
            };
            return Ok(Some(typed(found)));
        }
        // A member or call on a type written by name (`Helpers.log()`, `a.b.C.X`) or on a value whose declared type
        // is written: outside when the type is.
        let receiver_type = hit.receiver_type.as_deref().map(base_type);
        let written = match (hit.receiver.as_deref(), receiver_type.as_deref()) {
            (_, Some(t)) => Some(match t.rsplit_once('.') {
                Some((q, n)) => self.find_qualified(file, at, &nest, q, n).unwrap_or(Found::Unreached(Vec::new())),
                None => self.find_type(file, at, &nest, t),
            }),
            (Some(r), None) if is_type_name(r) => Some(self.find_type(file, at, &nest, r)),
            (Some(r), None) if r.contains('.') && !r.contains('(') => {
                let (q, n) = r.rsplit_once('.').expect("contains a dot");
                if is_type_name(n) { self.find_qualified(file, at, &nest, q, n) } else { None }
            }
            _ => None,
        };
        if written == Some(Found::Outside) {
            return Ok(Some(("unresolved", Vec::new())));
        }
        // On a value or type the rules pin to one repository type: that type's own member of the name. One it
        // inherits goes on to the shared resolution, which walks supertypes.
        if let Some(Found::Inside(ids)) = &written
            && let [id] = ids.as_slice()
            && matches!(hit.kind.as_str(), "call" | "member")
        {
            let members = members_of(conn, hit, *id)?;
            if let [one] = members.as_slice() {
                return Ok(Some(("exact", vec![*one])));
            }
        }
        // A bare name a static import brings: from the imported type, unless the caller's own types declare or
        // inherit a member of that name (they shadow it).
        if hit.receiver.is_none() && matches!(hit.kind.as_str(), "call" | "member") {
            let from_single: Vec<String> = file.static_single.get(&hit.name).cloned().unwrap_or_default();
            let from_any = !from_single.is_empty() || !file.static_on_demand.is_empty();
            if from_any {
                let chain = self.chain_names(file, &nest);
                let own = members_in(conn, &hit.name, &chain)?;
                if !own.is_empty() {
                    return Ok(None);
                }
                if !from_single.is_empty() {
                    // Single-static imports of the name: the members the repository's imported types declare. With
                    // one from outside as well, or several, the shared resolution decides among them.
                    let mut ids = Vec::new();
                    let mut outside = false;
                    for q in &from_single {
                        match self.qualified.get(q) {
                            Some(id) => ids.extend(members_of(conn, hit, *id)?),
                            None => outside = true,
                        }
                    }
                    return Ok(match ids.len() {
                        0 => Some(("unresolved", ids)),
                        1 if !outside => Some(("exact", ids)),
                        _ => None,
                    });
                }
                let mut ids = Vec::new();
                for q in &file.static_on_demand {
                    if let Some(id) = self.qualified.get(q) {
                        ids.extend(members_of(conn, hit, *id)?);
                    }
                }
                if ids.len() == 1 {
                    return Ok(Some(("exact", ids)));
                }
                if !ids.is_empty() {
                    return Ok(None);
                }
                // Neither the caller's types nor an imported repository type declares it: an outside type's member,
                // or a member inherited from outside.
                return Ok(Some(("unresolved", Vec::new())));
            }
        }
        Ok(None)
    }

    /// The shared resolution's answer for a Java reference, kept to what Java can mean by it.
    ///
    /// Fields and methods are separate namespaces (JLS 6.5): a member access (`this.modCount`) never means a method
    /// and a call never a field, though the shared resolution, matching names, may offer one. Kotlin keeps its
    /// properties and functions apart the same way. And a member of a value whose type isn't known is a candidate,
    /// never a link by its name alone (it may well be a library type's member).
    pub fn after(
        &self,
        conn: &Connection,
        hit: &RefHit,
        receiver_known: bool,
        result: (&'static str, Vec<i64>),
    ) -> rusqlite::Result<(&'static str, Vec<i64>)> {
        let (confidence, mut ids) = result;
        let wanted: &[&str] = match hit.kind.as_str() {
            "call" => &["method", "function"],
            "member" => &["property", "case"],
            _ => &[],
        };
        if !wanted.is_empty() && !ids.is_empty() {
            let mut stmt = conn.prepare_cached("SELECT kind FROM symbols WHERE id = ?1")?;
            let mut kept = Vec::new();
            for id in ids {
                let kind: Option<String> = stmt.query_row(params![id], |r| r.get(0)).optional()?;
                if kind.is_some_and(|k| wanted.contains(&k.as_str())) {
                    kept.push(id);
                }
            }
            ids = kept;
            if ids.is_empty() {
                return Ok(("unresolved", ids));
            }
        }
        let receiver = hit.receiver.as_deref();
        let self_like = matches!(receiver, Some("this" | "super"));
        if confidence == "unique_name" && receiver.is_some() && !self_like && !receiver_known {
            return Ok(("ambiguous", ids));
        }
        Ok((confidence, ids))
    }
}

/// A declared type's name without generic arguments or array brackets.
fn base_type(t: &str) -> String {
    use kc_extract::TypeExpr;
    match TypeExpr::decode(t) {
        Some(TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. }) => n,
        Some(TypeExpr::Array(e)) => base_type(&e.encode()),
        _ => t.split(['<', '[']).next().unwrap_or(t).trim().to_string(),
    }
}

/// `Name`, written as a type is: an upper-case first letter, and not a constant (`INSTANCE`, `MAX_VALUE`).
fn is_type_name(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
        && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && s.chars().any(char::is_lowercase)
}

/// The members of the type `id` itself a reference can mean: a method for a call, taking its number of arguments when
/// known; a field or enum constant for a member access.
fn members_of(conn: &Connection, hit: &RefHit, id: i64) -> rusqlite::Result<Vec<i64>> {
    let kinds = if hit.kind == "call" { "('method')" } else { "('property', 'case')" };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT id, min_args, max_args FROM symbols WHERE parent_id = ?2 AND name = ?1 AND name = ?1 COLLATE BINARY
           AND kind IN {kinds} ORDER BY id"
    ))?;
    let rows = stmt.query_map(params![hit.name, id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<u32>>(1)?, r.get::<_, Option<u32>>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, min, max) = row?;
        let fits = match (hit.args, min) {
            (Some(n), Some(min)) => n >= min && max.is_none_or(|max| n <= max),
            _ => true,
        };
        if fits {
            out.push(id);
        }
    }
    Ok(out)
}

/// Members named `name` whose type's simple name is one of `types`.
fn members_in(conn: &Connection, name: &str, types: &HashSet<String>) -> rusqlite::Result<Vec<i64>> {
    if types.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT s.id, s.container FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'java' AND s.name = ?1 AND s.container IS NOT NULL AND s.kind IN ('method', 'property', 'case')",
    )?;
    let rows = stmt.query_map(params![name], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, container) = row?;
        if types.contains(container.rsplit('.').next().unwrap_or(&container)) {
            out.push(id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names_and_bases() {
        assert!(is_type_name("Collections"));
        assert!(!is_type_name("MAX_VALUE"));
        assert!(!is_type_name("store"));
        assert_eq!(base_type("List"), "List");
    }
}
