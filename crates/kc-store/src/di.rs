//! Dependency-injection bindings: which concrete type the repo's container hands out for a service, read from the
//! registrations the C# extractor records (`RegisterType<Foo>().As<IFoo>()`, `AddScoped<IFoo, Foo>()`, …).
//!
//! A call through a service type (`_orders.Place()` with `IOrderService _orders`) resolves to the service's member.
//! When the repo's registrations outside tests bind that service to exactly one implementation, the call also
//! carries that implementation's member as `via_di`, so call trees go where the running code goes.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::query::{SymbolRow, Target};

/// `A.B.IFoo<Bar>` → `IFoo`.
pub fn bare_name(t: &str) -> String {
    let head = t.split('<').next().unwrap_or(t).trim().trim_end_matches('?');
    head.rsplit('.').next().unwrap_or(head).to_string()
}

pub use crate::is_test_path;

/// One registration of a service.
#[derive(Debug, Clone, Serialize)]
pub struct Registration {
    /// The service type as written; for `AsImplementedInterfaces`, each interface the implementation declares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The concrete type, when known; an assembly scan's name filter (`*Service`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
    /// `singleton`, `scoped`, `transient`, `unit_of_work`, or `default` (the container's).
    pub lifetime: String,
    /// `type`, `factory`, `instance` or `scan`; for Angular providers also `existing` (`useExisting`) and `parent` (a
    /// factory deferring to a parent injector's provider first, `implementation` what it otherwise makes).
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// Registered through `AsImplementedInterfaces`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub implemented_interfaces: bool,
    /// In test code: listed, but never used to bind a call.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// An Angular provider taken to apply where the call is because the call's file is in its directory (or below):
    /// a convention standing in for the injector tree (see [`ts_bound`]).
    #[serde(skip)]
    pub by_directory: bool,
    /// How a Microsoft DI registration is added, when not plainly (`try`, `enumerable`, `replace`: see
    /// `kc_extract::DiBinding::mode`): not a second implementation competing with the plain one.
    #[serde(skip)]
    pub mode: Option<String>,
}

impl Registration {
    /// A concrete implementation a call can be bound to: known, and not a scan's filter.
    pub(crate) fn concrete(&self) -> Option<&str> {
        self.implementation.as_deref().filter(|i| self.kind != "scan" && !i.contains('*'))
    }
}

/// The implementation a call through a service type is bound to.
#[derive(Debug, Clone, Serialize)]
pub struct Bound {
    /// The implementation's member.
    #[serde(flatten)]
    pub target: Target,
    pub lifetime: String,
    /// Where the binding is registered, `file:line`.
    pub registered: String,
    /// What the binding did not check: `directory_convention` for an Angular provider placed by its directory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<crate::answer::Assumption>,
}

const COLS: &str = "d.service, d.implementation, d.lifetime, d.kind, f.path, d.line, d.mode";

fn registration(r: &rusqlite::Row) -> rusqlite::Result<Registration> {
    let file: String = r.get(4)?;
    Ok(Registration {
        by_directory: false,
        service: r.get(0)?,
        implementation: r.get(1)?,
        lifetime: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "default".into()),
        kind: r.get(3)?,
        test: is_test_path(&file),
        file,
        line: r.get(5)?,
        implemented_interfaces: false,
        mode: r.get(6)?,
    })
}

/// Resolves the registrations whose factory makes what only the repo's declarations tell (`made`: a factory method's
/// call, a property): `implementation` becomes the type the factory's result has, and for one naming no service
/// (`service_made`: the container registers it as what it makes), the service too; both are cleared when it can't be
/// told. A factory returning the service type itself (`IFoo`) leaves the implementation unknown.
pub fn resolve_made(conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, u32, String, Option<String>, bool)> = conn
        .prepare(
            "SELECT d.rowid, f.path, d.line, d.made, d.service_name, d.service_made FROM di_bindings d JOIN files f ON f.id = d.file_id
             WHERE d.made IS NOT NULL ORDER BY d.rowid",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (rowid, file, line, made, service, service_made) in rows {
        let t = crate::query::made_type(conn, &file, line, &made)?;
        if service_made {
            conn.prepare_cached("UPDATE di_bindings SET implementation = ?1, service = ?1, service_name = ?1 WHERE rowid = ?2")?
                .execute(rusqlite::params![t, rowid])?;
        } else {
            let t = t.filter(|t| service.as_deref() != Some(t.as_str()));
            conn.prepare_cached("UPDATE di_bindings SET implementation = ?1 WHERE rowid = ?2")?.execute(rusqlite::params![t, rowid])?;
        }
    }
    Ok(())
}

/// Whether the repo registers something as `service` (a bare name).
pub fn provides(conn: &Connection, service: &str) -> Result<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM di_bindings WHERE service_name = ?1 LIMIT 1")?.exists([service])?)
}

/// Whether the repo registers anything.
pub fn any(conn: &Connection) -> Result<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM di_bindings LIMIT 1")?.exists([])?)
}

/// Registrations of a service, by its bare name: those naming it, and `AsImplementedInterfaces` ones whose
/// implementation declares it (when it is an interface: defined here as one, or named `IFoo`).
pub fn registrations_of(conn: &Connection, service: &str) -> Result<Vec<Registration>> {
    let sql = format!(
        "SELECT {COLS} FROM di_bindings d JOIN files f ON f.id = d.file_id
         WHERE d.service_name = ?1 OR (d.service IS NULL AND d.kind != 'scan' AND d.implementation IN (
           SELECT s.name FROM relations r JOIN symbols s ON s.id = r.from_id WHERE r.target_name = ?1 COLLATE BINARY))
         ORDER BY f.path, d.line"
    );
    let rows: Vec<Registration> = conn.prepare_cached(&sql)?.query_map([service], registration)?.collect::<rusqlite::Result<_>>()?;
    if rows.iter().all(|r| r.service.is_some()) {
        return Ok(rows);
    }
    let interface = is_interface(conn, service)?;
    Ok(rows
        .into_iter()
        .filter(|r| r.service.is_some() || interface)
        .map(|r| match r.service {
            Some(_) => r,
            None => Registration { service: Some(service.to_string()), implemented_interfaces: true, ..r },
        })
        .collect())
}

/// Registrations with `implementation` as the concrete type; `AsImplementedInterfaces` ones expanded into one per
/// interface it declares.
pub fn registered_as(conn: &Connection, implementation: &str) -> Result<Vec<Registration>> {
    let sql = format!(
        "SELECT {COLS} FROM di_bindings d JOIN files f ON f.id = d.file_id WHERE d.implementation = ?1
         ORDER BY f.path, d.line"
    );
    let rows: Vec<Registration> =
        conn.prepare_cached(&sql)?.query_map([implementation], registration)?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if r.service.is_some() || r.kind == "scan" {
            out.push(r);
            continue;
        }
        for interface in interfaces_of(conn, implementation)? {
            out.push(Registration { service: Some(interface), implemented_interfaces: true, ..r.clone() });
        }
    }
    Ok(out)
}

/// The single implementation the repo's registrations outside tests bind `service` to, with the registration.
/// `None` when there is none, more than one, or one that is not known (a factory with no obvious `new`).
pub fn bound(conn: &Connection, service: &str) -> Result<Option<Registration>> {
    Ok(single(registrations_of(conn, service)?, service))
}

/// Of registrations of `service` (a bare name), the one binding it outside tests when they all bind the same
/// implementation; see [`bound`].
pub fn single(regs: Vec<Registration>, service: &str) -> Option<Registration> {
    let regs: Vec<Registration> = regs.into_iter().filter(|r| !r.test).collect();
    let implementation = regs.first().and_then(Registration::concrete)?;
    let single = regs.iter().all(|r| r.concrete() == Some(implementation));
    (single && implementation != service).then(|| regs[0].clone())
}

/// Of the registrations outside tests binding `service`, the one whose path shares the longest prefix with `file`'s:
/// in a repo hosting several applications, the registration of the application (or the project family, as
/// `src/Shop.Api.Data/` is to `src/Shop.Api.Service/`) that `file` belongs to.
pub fn registered_near(conn: &Connection, service: &str, file: &str) -> Result<Option<Registration>> {
    Ok(nearest(registrations_of(conn, service)?, file))
}

/// [`registered_near`] among `regs`.
pub fn nearest(regs: Vec<Registration>, file: &str) -> Option<Registration> {
    let shared = |r: &Registration| r.file.chars().zip(file.chars()).take_while(|(a, b)| a == b).count();
    // `max_by_key` keeps the last of equals; reversing keeps the first, as `bound` does.
    regs.into_iter().filter(|r| !r.test).rev().max_by_key(shared)
}

/// The type arguments of a service type as written, by bare name: `Some([Order])` for `IHandler<Shop.Order>`, `None`
/// for an open generic (`IRepo<>`, `IMap<,>`) or a type written without arguments.
pub fn type_args(written: &str) -> Option<Vec<String>> {
    let (_, rest) = written.split_once('<')?;
    let inner = rest.strip_suffix('>')?;
    let mut args = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in inner.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                args.push(bare_name(&inner[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(bare_name(&inner[start..]));
    (!args.iter().all(String::is_empty)).then_some(args)
}

/// Whether a registration of the service written `written` serves a call through that service closed with `args`
/// (bare names, `None` where unknown): the same arguments, an open generic (`typeof(IRepo<>)`), which serves any, or a
/// service written without arguments (an `AsImplementedInterfaces` registration, whose arguments aren't known).
pub fn closes(written: &str, args: &[Option<String>]) -> bool {
    match type_args(written) {
        None => true,
        Some(w) => w.len() == args.len() && w.iter().zip(args).all(|(w, a)| a.as_ref().is_none_or(|a| a == w)),
    }
}

/// For a TypeScript call through `service` from code in `file` (of the class `caller`), the implementations the
/// Angular injector there may hand out, from the registrations of `service`, and of the `InjectionToken`s typed with it
/// (`new InjectionToken<Service>(…)`), outside tests.
///
/// A provider applies where its injector does: a module, component or route's providers serve what it declares, which
/// by convention lives in its directory or below, and the services provided in the same `providers` list as the
/// caller (a component providing a facade and the coordinator injecting it). Of the registrations in scope, the deepest
/// win; a class providing itself there (`providers: [Facade]`) is that class. With none in scope, a concrete class is
/// itself, and an abstract class, interface or token may be any of its registrations.
///
/// The providers the code itself places at the call (the caller's own file's `providers`, the list providing the
/// caller's class) take precedence over those its directory places there; one placed only by its directory is
/// `by_directory`, and the binding says `directory_convention`. Module and standalone `imports` are not read: which
/// injector a lazy route or an imported module provides into is the directory's guess.
///
/// A factory deferring to a parent injector (`inject(X, { skipSelf: true }) ?? …`) brings in the providers outside its
/// scope (any may be the parent) and what it falls back to. With no registration at all, an abstract class's one
/// `@Injectable({ providedIn: 'root' })` subclass. `useExisting` aliases are followed where they are provided. One
/// registration per implementation, `service` itself and unknown ones (a factory returning something else) left out:
/// one means the binding is known, several that any of them may run.
pub fn ts_bound(conn: &Connection, service: &str, file: &str, caller: Option<&str>) -> Result<Vec<Registration>> {
    let mut out: Vec<Registration> = Vec::new();
    // Registrations in the order found, so a direct one names an implementation before an alias reaching it.
    let mut todo: std::collections::VecDeque<(Registration, u8)> =
        ts_providers(conn, service, file, caller)?.into_iter().map(|r| (r, 0)).collect();
    let mut aliases: Vec<String> = vec![service.to_string()];
    while let Some((r, hops)) = todo.pop_front() {
        if r.kind == "existing" {
            // `{ provide: X, useExisting: TOKEN }`: what the token is bound to where that provider is; an alias met
            // before (the token and its class aliasing each other) adds nothing.
            let Some(alias) = r.implementation.clone() else { continue };
            if aliases.contains(&alias) {
                continue;
            }
            aliases.push(alias.clone());
            let next = if hops < 3 { ts_providers(conn, &alias, &r.file, None)? } else { Vec::new() };
            if next.is_empty() {
                // An alias of a class no provider rebinds: the class itself.
                todo.push_back((Registration { kind: "type".into(), ..r }, hops + 1));
            } else {
                let at = |n: Registration| Registration { service: r.service.clone(), file: r.file.clone(), line: r.line, by_directory: r.by_directory || n.by_directory, ..n };
                todo.extend(next.into_iter().map(|n| (at(n), hops + 1)));
            }
            continue;
        }
        if r.concrete().is_some_and(|i| i != service) && !out.iter().any(|o| o.implementation == r.implementation) {
            out.push(r);
        }
    }
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(out)
}

/// The providers of `service` (a class or token) that may apply at `file` for code of `caller`; see [`ts_bound`].
fn ts_providers(conn: &Connection, service: &str, file: &str, caller: Option<&str>) -> Result<Vec<Registration>> {
    let mut names = vec![service.to_string()];
    let tokens: Vec<String> = conn
        .prepare_cached(
            // From the few `new InjectionToken` sites (by name) to the constants holding them: not every top-level symbol.
            "SELECT DISTINCT s.name FROM refs r JOIN symbols s ON s.id = r.from_id
             WHERE r.name = 'InjectionToken' AND r.kind = 'instantiate' AND s.type_name = ?1 AND s.kind = 'property'
             AND s.container IS NULL
             ORDER BY s.name",
        )?
        .query_map([service], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names.extend(tokens);
    let mut regs = Vec::new();
    for n in &names {
        // Stories and test doubles outside test directories aren't what runs.
        regs.extend(registrations_of(conn, n)?.into_iter().filter(|r| {
            !r.test && !r.file.contains(".stories.") && !r.implementation.as_deref().is_some_and(is_double)
        }));
    }
    let own = |r: &Registration| r.implementation.as_deref() == r.service.as_deref().map(bare_name).as_deref();
    let concrete = is_concrete_class(conn, service)?;
    if regs.iter().all(own) {
        return Ok(if concrete { Vec::new() } else { abstract_subclass(conn, service)?.into_iter().collect() });
    }
    // The files whose providers list the caller's class: their injector builds it, and serves what it injects.
    let co_provided: Vec<String> = match caller {
        Some(c) => registrations_of(conn, c)?.into_iter().filter(|r| !r.test && own(r)).map(|r| r.file).collect(),
        None => Vec::new(),
    };
    let dir = |f: &str| f.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
    let here = dir(file);
    let in_scope = |r: &Registration| {
        let d = dir(&r.file);
        d.is_empty() || here == d || here.starts_with(&format!("{d}/")) || co_provided.contains(&r.file)
    };
    // Placed at the call by the code itself rather than by the directory it is in: the nearest injector, which wins
    // over a provider its directory places beside it.
    let local = |r: &Registration| r.file == file || co_provided.contains(&r.file);
    let regs: Vec<Registration> = if regs.iter().any(|r| in_scope(r) && local(r) && !own(r)) {
        regs.into_iter().filter(|r| !in_scope(r) || local(r) || own(r)).collect()
    } else {
        regs
    };
    // The root injector's providers apply everywhere, whatever the directory.
    let roots = root_provider_files(conn)?;
    let explicit = |r: &Registration| local(r) || roots.contains(&r.file);
    // Depth of a provider's scope: a component providing the caller's class is nearest of all.
    let depth = |r: &Registration| if co_provided.contains(&r.file) { usize::MAX } else { dir(&r.file).len() };
    let named = |r: &Registration| Registration { service: Some(service.to_string()), by_directory: !explicit(r), ..r.clone() };
    let Some(deepest) = regs.iter().filter(|r| in_scope(r)).map(depth).max() else {
        return Ok(if concrete { Vec::new() } else { regs.iter().filter(|r| !own(r)).map(named).collect() });
    };
    let nearest: Vec<&Registration> = regs.iter().filter(|r| in_scope(r) && depth(r) == deepest).collect();
    // The class provided as itself there: that is what runs.
    if nearest.iter().any(|r| own(r) && r.service.as_deref().map(bare_name).as_deref() == Some(service)) {
        return Ok(Vec::new());
    }
    let mut out: Vec<Registration> = Vec::new();
    for r in nearest.into_iter().filter(|r| !own(r)) {
        if r.kind != "parent" {
            out.push(named(r));
            continue;
        }
        // A parent's provider, from outside this scope, else the fallback.
        out.extend(regs.iter().filter(|o| !in_scope(o) && o.kind != "parent" && !own(o)).map(named));
        if r.implementation.is_some() {
            out.push(Registration { kind: "factory".into(), ..named(r) });
        }
    }
    Ok(out)
}

/// The files whose providers are the root injector's: those a file bootstrapping the application
/// (`bootstrapApplication(App, appConfig)`, `bootstrapModule(AppModule)`) imports by a relative path.
fn root_provider_files(conn: &Connection) -> Result<Vec<String>> {
    let rows: Vec<(String, String)> = conn
        .prepare_cached(
            // From the few bootstrap calls (by name) to their files' imports: not every relative import's file searched.
            "SELECT f.path, i.module FROM imports i JOIN files f ON f.id = i.file_id
             WHERE i.module LIKE '.%' AND i.file_id IN (SELECT r.file_id FROM refs r WHERE r.kind = 'call'
                                                        AND r.name IN ('bootstrapApplication', 'bootstrapModule'))
             ORDER BY f.path, i.module",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (from, module) in rows {
        let mut parts: Vec<&str> = from.rsplit_once('/').map_or(Vec::new(), |(d, _)| d.split('/').collect());
        for p in module.split('/') {
            match p {
                "." | "" => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        let path = parts.join("/");
        out.push(format!("{path}.ts"));
        out.push(format!("{path}/index.ts"));
    }
    Ok(out)
}

/// Whether `name` is a class of the repo that isn't abstract: injecting it gets the class unless a provider in scope
/// says otherwise.
fn is_concrete_class(conn: &Connection, name: &str) -> Result<bool> {
    let kinds: Vec<(String, String)> = conn
        .prepare_cached("SELECT kind, signature FROM symbols WHERE name = ?1 AND name = ?1 COLLATE BINARY AND kind IN ('class', 'protocol')")?
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(!kinds.is_empty() && kinds.iter().all(|(k, sig)| k == "class" && !format!(" {sig}").contains(" abstract class ")))
}

/// `MockOrders`, `OrdersServiceMock`, `FakeClock`, `ClockStub`: a test double by its name.
pub(crate) fn is_double(name: &str) -> bool {
    double_of(name).is_some()
}

/// The name a test double's name says it stands in for: `Orders` for `MockOrders`, `OrdersService` for
/// `OrdersServiceMock` or `OrdersServiceMocks`, `Clock` for `FakeClock`. `None` for a name without such an affix.
pub(crate) fn double_of(name: &str) -> Option<&str> {
    ["Mock", "Fake", "Stub", "Dummy"].iter().find_map(|w| {
        name.strip_prefix(w)
            .or_else(|| name.strip_suffix(w))
            .or_else(|| name.strip_suffix('s').and_then(|n| n.strip_suffix(w)))
    })
}

/// The one `@Injectable({ providedIn: 'root' })` subclass of `service`, when it is an abstract class.
fn abstract_subclass(conn: &Connection, service: &str) -> Result<Option<Registration>> {
    let is_abstract: bool = conn
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM symbols WHERE name = ?1 AND name = ?1 COLLATE BINARY AND kind = 'class'
             AND ' ' || signature LIKE '% abstract class %')",
        )?
        .query_row([service], |r| r.get(0))?;
    if !is_abstract {
        return Ok(None);
    }
    let sql = format!(
        "SELECT {COLS} FROM di_bindings d JOIN files f ON f.id = d.file_id
         WHERE d.service_name = d.implementation AND d.lifetime = 'singleton' AND d.implementation IN (
           SELECT s.name FROM relations r JOIN symbols s ON s.id = r.from_id WHERE r.target_name = ?1 COLLATE BINARY)
         ORDER BY f.path, d.line"
    );
    let subs: Vec<Registration> =
        conn.prepare_cached(&sql)?.query_map([service], registration)?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter(|r| !r.test).collect();
    Ok(match subs.as_slice() {
        [one] => Some(Registration { service: Some(service.to_string()), ..one.clone() }),
        _ => None,
    })
}

fn is_interface(conn: &Connection, name: &str) -> Result<bool> {
    let kinds: Vec<String> = conn
        .prepare_cached("SELECT kind FROM symbols WHERE name = ?1 AND kind IN ('class', 'struct', 'protocol')")?
        .query_map([name], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(if kinds.is_empty() { looks_like_interface(name) } else { kinds.iter().any(|k| k == "protocol") })
}

/// The interfaces a type declares directly.
fn interfaces_of(conn: &Connection, ty: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT r.target_name,
           (SELECT MAX(t.kind = 'protocol') FROM symbols t WHERE t.name = r.target_name AND t.kind IN ('class', 'struct', 'protocol'))
         FROM relations r JOIN symbols s ON s.id = r.from_id WHERE s.name = ?1 COLLATE BINARY ORDER BY r.line, r.target_name",
    )?;
    let rows: Vec<(String, Option<bool>)> = stmt.query_map([ty], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter(|(name, protocol)| match protocol {
            Some(p) => *p,
            None => looks_like_interface(name),
        })
        .map(|(name, _)| name)
        .collect())
}

/// `IFoo`: the .NET interface naming convention.
fn looks_like_interface(name: &str) -> bool {
    let mut c = name.chars();
    c.next() == Some('I') && c.next().is_some_and(char::is_uppercase)
}

/// What the repo's container knows about a type, for `context`.
#[derive(Debug, Clone, Serialize)]
pub struct DiSection {
    /// Registrations exposing the type as a service.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub registrations: Vec<Registration>,
    /// Registrations with the type as the implementation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub registered_as: Vec<Registration>,
    /// For a service: the implementations its registrations outside tests bind it to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bound: Vec<String>,
    /// Constructors taking the type as a parameter: where it is injected.
    pub injection_sites: usize,
    /// `unregistered` (an interface injected somewhere with no registration in the repo) and `conflicting: A, B`
    /// (one service registered with several implementations outside tests, by plain registrations).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
}

/// The registrations reaching the type named `ty` through its hierarchy, at most `limit`: those of its derived
/// interfaces as services, and those of its implementations and subclasses (direct or through another type) as
/// implementations; not those naming `ty` as the service (the type's own, in [`section`]). Each as
/// `file:line service → implementation (lifetime[, test])`, in file order; a last entry says how many more.
pub fn through_hierarchy(conn: &Connection, ty: &str, limit: usize) -> Result<Vec<String>> {
    if !any(conn)? {
        return Ok(Vec::new());
    }
    let direct: Vec<(String, String)> = conn
        .prepare_cached(
            "SELECT DISTINCT s.name, s.kind FROM relations r JOIN symbols s ON s.id = r.from_id
             WHERE r.target_name = ?1 COLLATE BINARY AND s.kind IN ('class', 'struct', 'protocol') ORDER BY s.name",
        )?
        .query_map([ty], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let (indirect, _) = crate::query::indirect_conformers(conn, ty, crate::query::INDIRECT_READ)?;
    let mut regs: Vec<Registration> = Vec::new();
    let mut names: Vec<(String, bool)> = direct.into_iter().map(|(n, k)| (n, k == "protocol")).collect();
    names.extend(indirect.iter().map(|i| (bare_name(&i.name), i.kind == "protocol")));
    let mut seen = std::collections::HashSet::new();
    for (n, interface) in names {
        if !seen.insert(n.clone()) {
            continue;
        }
        if interface {
            regs.extend(registrations_of(conn, &n)?);
        }
        regs.extend(registered_as(conn, &n)?);
    }
    regs.retain(|r| r.service.as_deref().map(bare_name).as_deref() != Some(ty));
    regs.sort_by(|a, b| (&a.file, a.line, &a.service).cmp(&(&b.file, b.line, &b.service)));
    regs.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.service == b.service);
    let total = regs.len();
    let mut out: Vec<String> = regs
        .iter()
        .take(limit)
        .map(|r| {
            let service = r.service.as_deref().unwrap_or("(scanned)");
            let made = r.implementation.as_deref().unwrap_or("(factory)");
            let what = if bare_name(service) == made { made.to_string() } else { format!("{service} → {made}") };
            format!("{}:{} {what} ({}{})", r.file, r.line, r.lifetime, if r.test { ", test" } else { "" })
        })
        .collect();
    if total > out.len() {
        out.push(format!("… {} more", total - out.len()));
    }
    Ok(out)
}

/// The DI section for a type, or `None` when the repo registers nothing, or nothing about this type.
pub fn section(conn: &Connection, symbol: &SymbolRow) -> Result<Option<DiSection>> {
    if !matches!(symbol.kind.as_str(), "class" | "struct" | "protocol") || !symbol.file.ends_with(".cs") {
        return Ok(None);
    }
    if !any(conn)? {
        return Ok(None);
    }
    let name = symbol.name.as_str();
    let registrations = registrations_of(conn, name)?;
    let registered_as = registered_as(conn, name)?;
    let mut stmt = conn.prepare_cached(
        "SELECT COUNT(DISTINCT r.from_id) FROM refs r JOIN symbols s ON s.id = r.from_id
         WHERE r.name = ?1 AND r.name = ?1 COLLATE BINARY AND r.kind = 'type_ref' AND s.kind = 'init'",
    )?;
    let injection_sites: usize = stmt.query_row([name], |r| r.get::<_, i64>(0))? as usize;
    if registrations.is_empty() && registered_as.is_empty() && (symbol.kind != "protocol" || injection_sites == 0) {
        return Ok(None);
    }

    let mut bound: Vec<String> = Vec::new();
    let mut flags = Vec::new();
    for r in registrations.iter().filter(|r| !r.test) {
        if let Some(i) = r.implementation.as_ref().filter(|i| *i != name)
            && !bound.contains(i)
        {
            bound.push(i.clone());
        }
    }
    if symbol.kind == "protocol" && registrations.iter().all(|r| r.test) && injection_sites > 0 {
        flags.push("unregistered".to_string());
    }
    // Conflicts per service as written: `IValidator<A>` and `IValidator<B>` are different services. Among plain
    // registrations only: a `TryAdd*` one stands back for any other, `TryAddEnumerable` adds one of several meant to
    // be resolved together, and `Replace` takes the place of the one before.
    let mut by_written: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in registrations.iter().filter(|r| !r.test && r.mode.is_none()) {
        if let (Some(s), Some(i)) = (r.service.as_deref(), r.concrete()) {
            let v = by_written.entry(s).or_default();
            if !v.contains(&i) {
                v.push(i);
            }
        }
    }
    let mut conflicts: Vec<(&str, Vec<&str>)> = by_written.into_iter().filter(|(_, v)| v.len() > 1).collect();
    conflicts.sort();
    for (service, impls) in conflicts {
        flags.push(format!("conflicting: {service} → {}", impls.join(", ")));
    }
    Ok(Some(DiSection { registrations, registered_as, bound, injection_sites, flags }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_names_and_test_paths() {
        assert_eq!(bare_name("A.B.IValidator<Order>"), "IValidator");
        assert_eq!(bare_name("IFoo"), "IFoo");
        assert!(is_test_path("tests/Api.Tests/Hooks/Setup.cs"));
        assert!(is_test_path("src/Api.UnitTests/Setup.cs"));
        assert!(!is_test_path("src/Api/Startup.cs"));
        assert!(!is_test_path("src/Jobs/DeleteStaleCartsJob.cs"));
        assert!(!is_test_path("src/Attestation/AttestationModule.cs"));
        assert!(looks_like_interface("IFoo") && !looks_like_interface("Item"));
    }

    #[test]
    fn generic_services_keep_their_arguments() {
        let s = |v: &[&str]| v.iter().map(|a| Some(a.to_string())).collect::<Vec<_>>();
        assert_eq!(type_args("IHandler<Shop.Order>"), Some(vec!["Order".to_string()]));
        assert_eq!(type_args("IMap<Dictionary<string, int>, Order>"), Some(vec!["Dictionary".to_string(), "Order".to_string()]));
        assert_eq!(type_args("IRepo<>"), None);
        assert_eq!(type_args("IRepo<,>"), None);
        assert!(closes("IHandler<Order>", &s(&["Order"])) && !closes("IHandler<Invoice>", &s(&["Order"])));
        // Open generics and unknown arguments serve any.
        assert!(closes("IRepo<>", &s(&["Order"])) && closes("IHandler<Invoice>", &[None]) && closes("IHandler", &s(&["Order"])));
        assert!(!closes("IMap<A, B>", &s(&["A"])));
    }
}
