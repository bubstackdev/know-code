//! Package, target and dependency queries over parsed manifests.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetDepRow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub kind: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetRow {
    pub name: String,
    pub kind: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<TargetDepRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDepRow {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement: Option<String>,
    /// Absolute root of the repo a path dependency lands in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_repo: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageRow {
    pub kind: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub targets: Vec<TargetRow>,
    pub deps: Vec<PackageDepRow>,
}

/// Every package declared in the repo's manifests, with targets, products and dependencies.
pub fn packages(conn: &Connection) -> Result<Vec<PackageRow>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.kind, p.name, f.path, p.line FROM packages p JOIN files f ON f.id = p.file_id
         ORDER BY f.path, p.line",
    )?;
    let rows: Vec<(i64, String, String, String, u32)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    let mut target_stmt =
        conn.prepare_cached("SELECT id, name, kind, line FROM package_targets WHERE package_id = ?1 ORDER BY line, id")?;
    let mut tdep_stmt = conn.prepare_cached(
        "SELECT name, product, package, kind, line FROM target_deps WHERE target_id = ?1 ORDER BY line, rowid",
    )?;
    let mut dep_stmt = conn.prepare_cached(
        "SELECT name, source, location, requirement, resolved_repo, line FROM package_deps WHERE package_id = ?1 AND source NOT IN ('compile', 'using')
         ORDER BY line",
    )?;
    for (id, kind, name, file, line) in rows {
        let targets: Vec<(i64, String, String, u32)> = target_stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut target_rows = Vec::with_capacity(targets.len());
        for (tid, tname, tkind, tline) in targets {
            let deps = tdep_stmt
                .query_map(params![tid], |r| {
                    Ok(TargetDepRow { name: r.get(0)?, product: r.get(1)?, package: r.get(2)?, kind: r.get(3)?, line: r.get(4)? })
                })?
                .collect::<rusqlite::Result<_>>()?;
            target_rows.push(TargetRow { name: tname, kind: tkind, line: tline, deps });
        }
        let deps = dep_stmt
            .query_map([id], |r| {
                Ok(PackageDepRow {
                    name: r.get(0)?,
                    source: r.get(1)?,
                    location: r.get(2)?,
                    requirement: r.get(3)?,
                    resolved_repo: r.get(4)?,
                    line: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        out.push(PackageRow { kind, name, file, line, targets: target_rows, deps });
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoEdge {
    /// The depending repo's manifest and line.
    pub file: String,
    pub line: u32,
    pub package: String,
    pub dependency: String,
    /// The checkout answering for the dependency's repo (see the workspace's checkout policy).
    pub to_repo: String,
    /// The dependency repo's other indexed checkouts, by root: `to_repo` was chosen over them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_checkouts: Vec<String>,
    /// Products of the dependency that targets in this package use.
    pub products: Vec<String>,
    /// Files in the depending repo that import a module of the dependency (`path:line import Module`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub import_sites: Vec<String>,
    /// A registry dependency's version requirement, as written (`^1.2.0`, `1.4.0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirement: Option<String>,
    /// The version the package's manifest in `to_repo` declares, when it does (see `kc_index::revision`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What the edge did not check: `revision_unverified` for a registry dependency whose requirement the checkout's
    /// version isn't known to satisfy. Set by `kc_index::revision::check_edges`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<crate::answer::Assumption>,
    /// Linked by package name through a registry, not a path.
    #[serde(skip)]
    pub registry: bool,
}

/// Which repo declares each package name, across the indexed repos, and which checkout answers for each repo.
///
/// Ownership belongs to a repo identity (its normalized origin, else its root), not to a checkout: two checkouts of
/// one repo (a clone and a worktree) declaring a package are one owner. A name declared by more than one identity
/// has no owner: a registry dependency on it can't say which repo it means.
#[derive(Debug, Default, Clone)]
pub struct PackageOwners {
    owner: HashMap<String, Option<String>>,
    /// For each identity, the checkout answering for it and the others, by root.
    checkouts: HashMap<String, (String, Vec<String>)>,
}

impl PackageOwners {
    /// Records that the repo `identity` declares `package`.
    pub fn add(&mut self, package: &str, identity: &str) {
        if package.is_empty() {
            return;
        }
        self.owner
            .entry(package.to_ascii_lowercase())
            .and_modify(|owner| {
                if owner.as_deref() != Some(identity) {
                    *owner = None;
                }
            })
            .or_insert_with(|| Some(identity.to_string()));
    }

    /// Records the checkout answering for `identity` (see the workspace's checkout policy) and its other checkouts.
    /// An identity without one answers with itself, which is then a root.
    pub fn set_checkout(&mut self, identity: &str, root: &str, others: Vec<String>) {
        self.checkouts.insert(identity.to_string(), (root.to_string(), others));
    }

    /// The identity of the repo declaring `package`, when one does. Names match case-insensitively, as NuGet does.
    pub fn identity(&self, package: &str) -> Option<&str> {
        self.owner.get(&package.to_ascii_lowercase())?.as_deref()
    }

    /// The root of the checkout answering for the repo declaring `package`.
    pub fn get(&self, package: &str) -> Option<&str> {
        self.identity(package).map(|id| self.checkout(id))
    }

    /// The root of the checkout answering for `identity`.
    pub fn checkout<'a>(&'a self, identity: &'a str) -> &'a str {
        self.checkouts.get(identity).map_or(identity, |(r, _)| r.as_str())
    }

    /// The other checkouts of `identity`, by root.
    pub fn other_checkouts(&self, identity: &str) -> &[String] {
        self.checkouts.get(identity).map_or(&[], |(_, o)| o.as_slice())
    }
}

/// Dependencies that land in another repo, with the products used from them: path dependencies by where they
/// point, registry dependencies by which indexed repo declares the package (answered by that repo's chosen
/// checkout). `root` and `identity` are the depending checkout's: a dependency on its own repo is no edge.
pub fn repo_edges(packages: &[PackageRow], root: &str, identity: &str, owners: &PackageOwners) -> Vec<RepoEdge> {
    let mut edges = Vec::new();
    for p in packages {
        for d in &p.deps {
            let (to, others) = match d.source.as_str() {
                "registry" => match owners.identity(&d.name).filter(|id| *id != identity) {
                    Some(id) => (owners.checkout(id), owners.other_checkouts(id).to_vec()),
                    None => continue,
                },
                _ => match d.resolved_repo.as_deref() {
                    Some(r) => (r, Vec::new()),
                    None => continue,
                },
            };
            if to == root {
                continue;
            }
            let mut products: Vec<String> = p
                .targets
                .iter()
                .flat_map(|t| &t.deps)
                .filter(|td| td.package.as_deref().is_some_and(|pk| pk.eq_ignore_ascii_case(&d.name)))
                .map(|td| td.product.clone().unwrap_or_else(|| td.name.clone()))
                .collect();
            products.sort();
            products.dedup();
            edges.push(RepoEdge {
                file: p.file.clone(),
                line: d.line,
                package: p.name.clone(),
                dependency: d.name.clone(),
                to_repo: to.to_string(),
                other_checkouts: others,
                products,
                import_sites: Vec::new(),
                requirement: d.requirement.clone().filter(|_| d.source == "registry"),
                version: None,
                assumptions: Vec::new(),
                registry: d.source == "registry",
            });
        }
    }
    edges
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestHit {
    pub name: String,
    /// `package`, or the target kind (`target`, `product:library`, `application`, …).
    pub kind: String,
    pub package: String,
    pub file: String,
    pub line: u32,
}

/// Packages, targets and products whose name starts with `prefix` (case-insensitive), exact matches first, at most
/// `limit`; and how many match (at most [`FIND_READ`]: a lower bound past it).
pub fn find(conn: &Connection, prefix: &str, limit: usize) -> Result<(Vec<ManifestHit>, usize)> {
    let pattern = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = conn.prepare_cached(
        "SELECT p.name, 'package', p.name, f.path, p.line FROM packages p JOIN files f ON f.id = p.file_id
         WHERE p.name LIKE ?1 ESCAPE '\\'
         UNION ALL
         SELECT t.name, t.kind, p.name, f.path, t.line FROM package_targets t JOIN packages p ON p.id = t.package_id
         JOIN files f ON f.id = t.file_id WHERE t.name LIKE ?1 ESCAPE '\\'
         ORDER BY 1, 2, 4, 5 LIMIT 500",
    )?;
    let mut hits: Vec<ManifestHit> = stmt
        .query_map([pattern], |r| {
            Ok(ManifestHit { name: r.get(0)?, kind: r.get(1)?, package: r.get(2)?, file: r.get(3)?, line: r.get(4)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    hits.sort_by_key(|h| (!h.name.eq_ignore_ascii_case(prefix), h.kind != "package", h.file.clone(), h.line));
    let total = hits.len();
    hits.truncate(limit);
    Ok((hits, total))
}

/// Manifest matches [`find`] reads at most.
pub const FIND_READ: usize = 500;

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(name: &str, source: &str, resolved: Option<&str>) -> PackageDepRow {
        PackageDepRow {
            name: name.into(),
            source: source.into(),
            location: String::new(),
            requirement: None,
            resolved_repo: resolved.map(str::to_string),
            line: 3,
        }
    }

    #[test]
    fn registry_dependencies_resolve_to_the_repo_declaring_the_package() {
        let mut owners = PackageOwners::default();
        owners.add("Acme.Common.Data", "/c/common");
        owners.add("Acme.Cart", "/c/cart");
        // Declared twice across repos (a fork, a scaffold): no owner.
        owners.add("@acme/forked", "/c/a");
        owners.add("@acme/forked", "/c/b");
        // Declared twice by one repo (a library and its workspace root): still that repo.
        owners.add("@acme/ui", "/c/ui");
        owners.add("@acme/ui", "/c/ui");
        // Declared by two checkouts of one repo: one owner, answered by its chosen checkout.
        owners.add("Acme.Lib", "example.test/group/lib");
        owners.add("Acme.Lib", "example.test/group/lib");
        owners.set_checkout("example.test/group/lib", "/c/lib-worktree", vec!["/c/lib".into()]);
        assert_eq!(owners.get("acme.common.data"), Some("/c/common"));
        assert_eq!(owners.get("@acme/forked"), None);
        assert_eq!(owners.get("@acme/ui"), Some("/c/ui"));
        assert_eq!(owners.get("acme.lib"), Some("/c/lib-worktree"));

        let pkg = PackageRow {
            kind: "csproj".into(),
            name: "Acme.Cart".into(),
            file: "src/Acme.Cart.csproj".into(),
            line: 1,
            targets: Vec::new(),
            deps: vec![
                dep("Acme.Common.Data", "registry", None),
                dep("Newtonsoft.Json", "registry", None),
                dep("@acme/forked", "registry", None),
                dep("Acme.Cart.Service", "path", Some("/c/cart")),
                dep("Acme.Cart", "registry", None),
                dep("Shared", "path", Some("/c/shared")),
                dep("Acme.Lib", "registry", None),
            ],
        };
        let edges: Vec<(String, String)> =
            repo_edges(&[pkg], "/c/cart", "/c/cart", &owners).into_iter().map(|e| (e.dependency, e.to_repo)).collect();
        assert_eq!(
            edges,
            [
                ("Acme.Common.Data".to_string(), "/c/common".to_string()),
                ("Shared".into(), "/c/shared".into()),
                ("Acme.Lib".into(), "/c/lib-worktree".into())
            ]
        );
    }
}
