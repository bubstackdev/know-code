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
use kc_extract::{PathRoot, Seg};

/// Whether a reference in this file resolves by Java's rules.
pub fn handles(file: &str) -> bool {
    file.ends_with(".java") || file.ends_with(".kt")
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
             WHERE f.lang IN ('java', 'kotlin') AND s.kind IN ('class', 'protocol', 'enum', 'struct', 'typealias')",
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
             WHERE f.lang IN ('java', 'kotlin') AND r.kind = 'inherits'",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (from, target) = row?;
            self.supers.entry(from).or_default().push(target);
        }
        Ok(())
    }

    fn scope(&mut self, conn: &Connection, file: &str) -> rusqlite::Result<&FileScope> {
        if !self.files.contains_key(file) {
            // The package the file declares (a Kotlin file of functions alone declares no type to carry it), else the
            // namespace of a type it declares.
            let declared: Option<String> =
                conn.query_row("SELECT package FROM files WHERE path = ?1", params![file], |r| r.get(0)).optional()?.flatten();
            let package = match declared {
                Some(p) => Some(p),
                None => conn
                    .query_row(
                        "SELECT json_extract(s.type_name, '$.ns') FROM symbols s JOIN files f ON f.id = s.file_id
                         WHERE f.path = ?1 AND s.parent_id IS NULL AND json_valid(s.type_name)
                           AND json_extract(s.type_name, '$.ns') IS NOT NULL LIMIT 1",
                        params![file],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?,
            };
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
                        // Kotlin's `import a.b.C as D` names `a.b.C` as `D` in the file.
                        let (q, alias) = match q.split_once(" as ") {
                            Some((q, a)) => (q.to_string(), Some(a.to_string())),
                            None => (q.clone(), None),
                        };
                        let simple = alias.unwrap_or_else(|| q.rsplit('.').next().unwrap_or(&q).to_string());
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
            // No repository type has the name, but an import may give it to one (Kotlin's `import a.b.C as D`).
            return match file.single.get(name) {
                Some(q) => self.qualified.get(q).map_or(Found::Outside, |id| Found::Inside(vec![*id])),
                None => Found::Outside,
            };
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
            // In Kotlin a top-level type's own name, in its body, is read through the file's scope, where an explicit
            // import of the name wins (`import org.lib.Loader` inside `class Loader` means the library's).
            let imported_over = container.is_none() && at.0.ends_with(".kt") && file.single.contains_key(name);
            if last == name
                && !imported_over
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

    /// The repository type a path names, step by step: each step is the current type's own member (or one a repository
    /// supertype declares) of that name, and its declared type, read where it is declared. `None` as soon as a step
    /// leaves the repository or isn't one declaration.
    fn path_type(&mut self, conn: &Connection, hit: &RefHit, root: &PathRoot, segs: &[Seg]) -> rusqlite::Result<Option<i64>> {
        let nest = Self::nest(hit);
        let package = self.files[&hit.file].package.clone();
        let caller = nest.last().and_then(|n| self.qualified.get(&package.as_ref().map_or_else(|| n.clone(), |p| format!("{p}.{n}"))).copied());
        let mut cur = match root {
            PathRoot::SelfType => caller,
            PathRoot::Static(name) => match self.find_type(&self.files[&hit.file], (hit.file.as_str(), hit.line), &nest, name) {
                Found::Inside(ids) if ids.len() == 1 => Some(ids[0]),
                _ => None,
            },
            PathRoot::Prop(name) => match caller {
                Some(c) => self.member_type(conn, c, name, false)?,
                None => None,
            },
            PathRoot::Expr(_) => None,
        };
        for seg in segs {
            let Some(t) = cur else { return Ok(None) };
            cur = match seg {
                Seg::Member(n) => self.member_type(conn, t, n, false)?,
                Seg::Call(n) => self.member_type(conn, t, n, true)?,
                Seg::Index => None,
            };
        }
        Ok(cur)
    }

    /// The repository type of the one member `name` the type `id` (or, failing that, a repository supertype) declares.
    fn member_type(&mut self, conn: &Connection, id: i64, name: &str, callable: bool) -> rusqlite::Result<Option<i64>> {
        let mut queue = vec![id];
        let mut seen = HashSet::new();
        while let Some(t) = queue.pop() {
            if !seen.insert(t) || seen.len() > 16 {
                continue;
            }
            let kinds = if callable { "('method')" } else { "('property', 'case')" };
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT s.type_name, s.container, f.path, s.kind FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.parent_id = ?1 AND s.name = ?2 AND s.name = ?2 COLLATE BINARY AND s.kind IN {kinds}"
            ))?;
            let rows: Vec<(Option<String>, Option<String>, String, String)> = stmt
                .query_map(params![t, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            match rows.as_slice() {
                [] => {}
                // An enum constant's type is its enum.
                [(_, _, _, kind)] if kind == "case" => return Ok(Some(t)),
                [(Some(ty), container, file, _)] => return self.declared_in(conn, ty, file, container.as_deref()),
                // Overloads with different types, or a declaration without one: not settled here.
                _ => return Ok(None),
            }
            let (file, container) = self.type_place(conn, t)?;
            self.scope(conn, &file)?;
            let nest = nest_of(container.as_deref(), self.names.get(&t).map(String::as_str));
            for s in self.supers.get(&t).cloned().unwrap_or_default() {
                if let Found::Inside(ids) = self.find_type(&self.files[&file], (file.as_str(), 0), &nest, &s)
                    && let [one] = ids.as_slice()
                {
                    queue.push(*one);
                }
            }
        }
        Ok(None)
    }

    /// The repository type a declared type names, read in the file and type that declare it.
    fn declared_in(&mut self, conn: &Connection, encoded: &str, file: &str, container: Option<&str>) -> rusqlite::Result<Option<i64>> {
        let Some(name) = base_type(encoded) else { return Ok(None) };
        self.load(conn)?;
        self.scope(conn, file)?;
        let nest = nest_of(container, None);
        let found = match name.rsplit_once('.') {
            Some((q, n)) => self.find_qualified(&self.files[file], (file, 0), &nest, q, n),
            None => Some(self.find_type(&self.files[file], (file, 0), &nest, &name)),
        };
        Ok(match found {
            Some(Found::Inside(ids)) if ids.len() == 1 => Some(ids[0]),
            _ => None,
        })
    }

    fn type_place(&self, conn: &Connection, id: i64) -> rusqlite::Result<(String, Option<String>)> {
        conn.query_row(
            "SELECT f.path, s.container FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// Resolves a reference in a Java file when Java's rules decide it; `None` leaves it to the shared resolution.
    pub fn scoped(&mut self, conn: &Connection, hit: &RefHit) -> rusqlite::Result<Option<(&'static str, Vec<i64>)>> {
        self.load(conn)?;
        self.scope(conn, &hit.file)?;
        let found = self.scoped_by_rules(conn, hit)?;
        // A reference never reaches a `private` declaration it can't see, whichever rule offered it.
        Ok(match found {
            Some((confidence, ids)) if !ids.is_empty() => {
                let before = ids.len();
                let ids = visible_ids(conn, hit, ids)?;
                Some(match (confidence, ids.len()) {
                    (_, 0) => ("unresolved", ids),
                    ("exact", _) if ids.len() < before => ("ambiguous", ids),
                    (c, _) => (c, ids),
                })
            }
            other => other,
        })
    }

    fn scoped_by_rules(&mut self, conn: &Connection, hit: &RefHit) -> rusqlite::Result<Option<(&'static str, Vec<i64>)>> {
        // `this` in a Kotlin extension's body is the extension's receiver. On the repository's type its own member
        // decides; on a library's, the members aren't known, so no repository member is a candidate, only a
        // repository extension on that type (and a member would still win over it).
        if hit.file.ends_with(".kt")
            && hit.receiver.as_deref() == Some("this")
            && matches!(hit.kind.as_str(), "call" | "member")
            && let Some(from) = &hit.from
            && let Some(receiver) = extension_receiver(&signature_of(conn, from.id)?, &from.name)
        {
            let file = &self.files[&hit.file];
            match self.find_type(file, (hit.file.as_str(), hit.line), &Self::nest(hit), &receiver) {
                Found::Inside(ids) if ids.len() == 1 => {
                    let members = members_of(conn, hit, ids[0])?;
                    return Ok(if let [one] = members.as_slice() { Some(("exact", vec![*one])) } else { None });
                }
                Found::Outside => {
                    let fitting: Vec<i64> =
                        extensions_named(conn, hit)?.into_iter().filter(|(_, r)| *r == receiver).map(|(id, _)| id).collect();
                    return Ok(Some(if fitting.is_empty() { ("unresolved", fitting) } else { ("ambiguous", fitting) }));
                }
                Found::Inside(_) | Found::Unreached(_) => {}
            }
        }
        // A receiver the extractor wrote as a path (`builder().setX(…)`, `this.store`, `Type.FIELD`): its type,
        // followed through the repository's declarations by Java's rules.
        let path_type = match hit.receiver_type.as_deref().and_then(kc_extract::TypeExpr::decode) {
            Some(kc_extract::TypeExpr::Path { root, segs }) if matches!(hit.kind.as_str(), "call" | "member") => {
                self.path_type(conn, hit, &root, &segs)?
            }
            _ => None,
        };
        let file = &self.files[&hit.file];
        let nest = Self::nest(hit);
        let at = (hit.file.as_str(), hit.line);
        if let Some(t) = path_type {
            let members = members_of(conn, hit, t)?;
            if let [one] = members.as_slice() {
                return Ok(Some(("exact", vec![*one])));
            }
        }
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
        let receiver_type = hit.receiver_type.as_deref().and_then(base_type);
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
        // A Kotlin extension declared in the repository applies to a receiver of the type it names, the repository's or
        // a library's (`Int::class.asTypeName()`): only where that type fits, never by its name alone.
        if hit.file.ends_with(".kt")
            && hit.receiver.as_deref().is_some_and(|r| r != "this" && r != "super")
            && matches!(hit.kind.as_str(), "call" | "member")
        {
            let extensions = extensions_named(conn, hit)?;
            if !extensions.is_empty() {
                let receiver_name = receiver_type
                    .clone()
                    .or_else(|| path_type.and_then(|t| self.names.get(&t).cloned()))
                    .or_else(|| hit.receiver.as_deref().filter(|r| is_type_name(r)).map(str::to_string));
                let Some(want) = receiver_name else {
                    return Ok(Some(("ambiguous", extensions.into_iter().map(|(id, _)| id).collect())));
                };
                let want = want.rsplit('.').next().unwrap_or(&want).to_string();
                let fitting: Vec<i64> = extensions.iter().filter(|(_, r)| *r == want).map(|(id, _)| *id).collect();
                // A member always wins over an extension. Only a repository type's members are known, so an extension
                // is linked only on one; on a library type it is a candidate (the library type may declare the name).
                let own_receiver = matches!(written, Some(Found::Inside(_))) || path_type.is_some();
                match fitting.len() {
                    1 if own_receiver => return Ok(Some(("exact", fitting))),
                    0 => {}
                    _ => return Ok(Some(("ambiguous", fitting))),
                }
            }
        }
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
        // In a Kotlin extension's body a bare name is first the receiver's member (`fun KmClass.isEnum() = kind == …`
        // reads `KmClass.kind`). On the repository's type its own member decides; a library type's members aren't known,
        // so any declaration of the name is a candidate, never a link.
        if hit.file.ends_with(".kt")
            && hit.receiver.is_none()
            && matches!(hit.kind.as_str(), "call" | "member")
            && let Some(from) = &hit.from
            && let Some(receiver) = extension_receiver(&signature_of(conn, from.id)?, &from.name)
        {
            match self.find_type(file, at, &nest, &receiver) {
                Found::Inside(ids) if ids.len() == 1 => {
                    let members = members_of(conn, hit, ids[0])?;
                    if let [one] = members.as_slice() {
                        return Ok(Some(("exact", vec![*one])));
                    }
                }
                Found::Inside(_) => {}
                Found::Outside | Found::Unreached(_) => {
                    let named = named_in_family(conn, hit)?;
                    return Ok(Some(if named.is_empty() { ("unresolved", named) } else { ("ambiguous", named) }));
                }
            }
        }
        // A bare Kotlin name, outside an extension's body: the caller's own (or inherited) members first, then a
        // top-level declaration an import names, then one of the file's own package, then of a star-imported package.
        // Anything else is Kotlin's default imports' (`require`, `emptyList`) or another library's: outside.
        if hit.file.ends_with(".kt") && hit.receiver.is_none() && matches!(hit.kind.as_str(), "call" | "member") {
            // A class's companion object's members are in scope in its body, before anything top-level.
            let mut companions = Vec::new();
            for outer in nest.iter().rev() {
                companions.extend(members_with_container(conn, hit, &format!("{outer}.Companion"))?);
                if !companions.is_empty() {
                    break;
                }
            }
            if let [one] = companions.as_slice() {
                return Ok(Some(("exact", vec![*one])));
            }
            let chain = self.chain_names(file, &nest);
            if !companions.is_empty() || !members_in(conn, &hit.name, &chain)?.is_empty() {
                return Ok(None);
            }
            if let Some(q) = file.single.get(&hit.name) {
                let (package, _) = q.rsplit_once('.').unwrap_or(("", q));
                // `import a.b.C.member`: a type's member (a companion's or a Java static), or a package's top-level.
                if let Some(t) = self.qualified.get(package) {
                    let ids = members_of(conn, hit, *t)?;
                    return Ok(Some(if ids.len() == 1 { ("exact", ids) } else if ids.is_empty() { ("unresolved", ids) } else { ("ambiguous", ids) }));
                }
                let ids = top_level(conn, hit, Some(package))?;
                return Ok(Some(match ids.len() {
                    0 => ("unresolved", ids),
                    1 => ("exact", ids),
                    _ => ("ambiguous", ids),
                }));
            }
            let own = top_level(conn, hit, file.package.as_deref())?;
            match own.len() {
                1 => return Ok(Some(("exact", own))),
                0 => {}
                _ => return Ok(None),
            }
            let mut starred = Vec::new();
            for p in &file.on_demand {
                starred.extend(top_level(conn, hit, Some(p))?);
            }
            return Ok(Some(match starred.len() {
                0 => ("unresolved", starred),
                1 => ("exact", starred),
                _ => ("ambiguous", starred),
            }));
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
        if !ids.is_empty() {
            ids = visible_ids(conn, hit, ids)?;
            if ids.is_empty() {
                return Ok(("unresolved", ids));
            }
        }
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
        // On a receiver the query typed (`builder().setX(…).build()`), the type's own member wins over a supertype's
        // of the same name, as javac binds a call to the most specific declaration of the receiver's static type.
        if receiver_known
            && hit.receiver.as_deref() != Some("super")
            && let Some(t) = hit.receiver_type.as_deref()
            && !ids.is_empty()
        {
            let own = own_members(conn, hit, t)?;
            if let [one] = own.as_slice() {
                return Ok(("exact", vec![*one]));
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

/// The dotted names of the types code inside `container` (and, for a type's own body, the type `own`) is nested in,
/// outermost first.
fn nest_of(container: Option<&str>, own: Option<&str>) -> Vec<String> {
    let inner = match (container, own) {
        (Some(c), Some(o)) => format!("{c}.{o}"),
        (Some(c), None) => c.to_string(),
        (None, Some(o)) => o.to_string(),
        (None, None) => return Vec::new(),
    };
    let parts: Vec<&str> = inner.split('.').collect();
    (1..=parts.len()).map(|n| parts[..n].join(".")).collect()
}

/// A written type's name without generic arguments or array brackets; `None` for a type the query derives (a path
/// from a field or a call).
fn base_type(t: &str) -> Option<String> {
    use kc_extract::TypeExpr;
    match TypeExpr::decode(t)? {
        TypeExpr::Named(n) | TypeExpr::Generic { name: n, .. } => Some(n),
        TypeExpr::Array(e) => base_type(&e.encode()),
        _ => None,
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
        if fits(hit.args, min, max) {
            out.push(id);
        }
    }
    Ok(out)
}

/// The Java members named like the reference, of its kind and fitting its argument count, whose type is `container`
/// exactly (its dotted name within its package, as declarations record it).
fn own_members(conn: &Connection, hit: &RefHit, container: &str) -> rusqlite::Result<Vec<i64>> {
    let kinds = if hit.kind == "call" { "('method')" } else { "('property', 'case')" };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT s.id, s.min_args, s.max_args FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang IN ('java', 'kotlin') AND s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.container = ?2 AND s.kind IN {kinds}"
    ))?;
    let rows = stmt.query_map(params![hit.name, container], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<u32>>(1)?, r.get::<_, Option<u32>>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, min, max) = row?;
        if fits(hit.args, min, max) {
            out.push(id);
        }
    }
    Ok(out)
}

fn fits(args: Option<u32>, min: Option<u32>, max: Option<u32>) -> bool {
    match (args, min) {
        (Some(n), Some(min)) => n >= min && max.is_none_or(|max| n <= max),
        _ => true,
    }
}

/// Kotlin top-level declarations named like the reference (a function for a call, a property for a member use) in
/// the files of `package` (the default package for `None`), as each file's `package` header declares it.
fn top_level(conn: &Connection, hit: &RefHit, package: Option<&str>) -> rusqlite::Result<Vec<i64>> {
    let kinds = if hit.kind == "call" { "('function')" } else { "('property')" };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT s.id, s.signature, f.path FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'kotlin' AND s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.container IS NULL
           AND s.kind IN {kinds} AND f.package IS ?2 ORDER BY s.id"
    ))?;
    let rows = stmt.query_map(params![hit.name, package], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, signature, path) = row?;
        // An extension needs a receiver; a bare name never calls one (outside an extension's own body). A top-level
        // `private` declaration is seen only in its own file.
        let private = signature.split_whitespace().take_while(|w| !matches!(*w, "fun" | "val" | "var")).any(|w| w == "private");
        if extension_receiver(&signature, &hit.name).is_none() && (!private || path == hit.file) {
            out.push(id);
        }
    }
    Ok(out)
}

/// The Kotlin members named like the reference, of its kind, whose container is exactly `container`, in the reference's
/// own file (a companion object, as the class declaring it sees it).
fn members_with_container(conn: &Connection, hit: &RefHit, container: &str) -> rusqlite::Result<Vec<i64>> {
    let kinds = if hit.kind == "call" { "('function', 'method')" } else { "('property', 'case')" };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.path = ?3 AND s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.container = ?2 AND s.kind IN {kinds}"
    ))?;
    stmt.query_map(params![hit.name, container, hit.file], |r| r.get(0))?.collect()
}

/// The declarations of `ids` the reference can see. A `private` one, or one inside a `private` type, is seen only
/// within its own type (Kotlin) or top-level type (Java), so never from another file; the file is the check, as each
/// of those lies within one.
fn visible_ids(conn: &Connection, hit: &RefHit, ids: Vec<i64>) -> rusqlite::Result<Vec<i64>> {
    let mut place = conn.prepare_cached(
        "SELECT s.file_id, f.path, s.name, s.container, s.signature FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
    )?;
    let mut outer = conn.prepare_cached(
        "SELECT signature FROM symbols WHERE file_id = ?1 AND name = ?2 AND name = ?2 COLLATE BINARY AND container IS ?3",
    )?;
    let mut out = Vec::new();
    for id in ids {
        let row: Option<(i64, String, String, Option<String>, String)> =
            place.query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?;
        let Some((file_id, path, name, container, signature)) = row else { continue };
        if path == hit.file || !crate::jvm::handles(&path) {
            out.push(id);
            continue;
        }
        let mut hidden = is_private(&signature, &name);
        if let Some(container) = container.as_deref() {
            let parts: Vec<&str> = container.split('.').collect();
            for k in 1..=parts.len() {
                if hidden {
                    break;
                }
                let within = (k > 1).then(|| parts[..k - 1].join("."));
                let signatures: Vec<String> =
                    outer.query_map(params![file_id, parts[k - 1], within], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                hidden = signatures.iter().any(|s| is_private(s, parts[k - 1]));
            }
        }
        if !hidden {
            out.push(id);
        }
    }
    Ok(out)
}

/// Whether a declaration's signature carries `private` among the modifiers before its name.
fn is_private(signature: &str, name: &str) -> bool {
    signature
        .split_whitespace()
        .take_while(|w| w.starts_with('@') || (!w.starts_with(name) && !w.contains(['(', ':', '='])))
        .any(|w| w == "private")
}

fn signature_of(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row("SELECT signature FROM symbols WHERE id = ?1", params![id], |r| r.get(0)).optional().map(Option::unwrap_or_default)
}

/// Every Java and Kotlin declaration of the reference's name and namespace: the candidates when nothing narrows them.
fn named_in_family(conn: &Connection, hit: &RefHit) -> rusqlite::Result<Vec<i64>> {
    let kinds = if hit.kind == "call" { "('function', 'method')" } else { "('property', 'case')" };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang IN ('java', 'kotlin') AND s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind IN {kinds} ORDER BY s.id"
    ))?;
    stmt.query_map(params![hit.name], |r| r.get(0))?.collect()
}

/// The repository's Kotlin extensions named like the reference (a function for a call, a property for a member use),
/// with the simple name of the receiver type each declares (`fun Iterable<T>.joinToCode()` gives `Iterable`).
fn extensions_named(conn: &Connection, hit: &RefHit) -> rusqlite::Result<Vec<(i64, String)>> {
    let kinds = if hit.kind == "call" { "('function', 'method')" } else { "('property')" };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT s.id, s.signature FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang = 'kotlin' AND s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind IN {kinds}"
    ))?;
    let rows = stmt.query_map(params![hit.name], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, signature) = row?;
        if let Some(receiver) = extension_receiver(&signature, &hit.name) {
            out.push((id, receiver));
        }
    }
    Ok(out)
}

/// The receiver type's simple name in an extension's signature: `fun <T> Iterable<T>.joinToCode()`,
/// `val KClass<*>.isData`, `fun Foo.Bar?.x()`; `None` for a declaration that isn't an extension.
fn extension_receiver(signature: &str, name: &str) -> Option<String> {
    let at = signature.find(&format!(".{name}"))?;
    let head = &signature[..at];
    let keyword = head.rfind("fun ").or_else(|| head.rfind("val ")).or_else(|| head.rfind("var "))?;
    let mut ty = head[keyword + 4..].trim();
    if ty.starts_with('<') {
        // The declaration's own type parameters.
        let mut depth = 0;
        let end = ty.char_indices().find(|(_, c)| {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            depth == 0
        })?;
        ty = ty[end.0 + 1..].trim();
    }
    let base = ty.split('<').next()?.trim_end_matches('?').trim();
    let simple = base.rsplit('.').next()?;
    (!simple.is_empty() && simple.chars().next().is_some_and(char::is_uppercase)).then(|| simple.to_string())
}

/// Members named `name` whose type's simple name is one of `types`.
fn members_in(conn: &Connection, name: &str, types: &HashSet<String>) -> rusqlite::Result<Vec<i64>> {
    if types.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT s.id, s.container FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.lang IN ('java', 'kotlin') AND s.name = ?1 AND s.container IS NOT NULL AND s.kind IN ('method', 'property', 'case')",
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
        assert_eq!(base_type("List").as_deref(), Some("List"));
        assert_eq!(extension_receiver("public fun <T> Iterable<T>.joinToCode(): CodeBlock", "joinToCode").as_deref(), Some("Iterable"));
        assert_eq!(extension_receiver("val KClass<*>.isData: Boolean", "isData").as_deref(), Some("KClass"));
        assert_eq!(extension_receiver("public fun asTypeName(): TypeName", "asTypeName"), None);
        assert!(is_private("private data class Reference(", "Reference"));
        assert!(is_private("@Deprecated private static final int LIMIT = 3", "LIMIT"));
        assert!(!is_private("val text: String", "text"));
        assert!(!is_private("fun privateCount(): Int", "privateCount"));
    }
}
