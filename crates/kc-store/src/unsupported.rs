//! Constructs an extractor recognised but didn't read (`kc_extract::Unsupported`), as stored (table `unsupported`), and the answer's `incomplete` entry (reason `unsupported`) for those an answer draws on: an
//! answer that would otherwise say `complete: true` while what they register, link or include is missing from it.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::answer::{Incomplete, Reason};

/// One construct met but not read, where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Site {
    /// `di`, `bus`, `sql`, `ansible`, `terraform`, `ci`, `angular`, `imports` or `http` (endpoint registrations whose
    /// routes can't be placed: read back as `unsupported` route rows by `query::http_routes`, which derives more).
    pub area: String,
    pub construct: String,
    pub file: String,
    pub line: u32,
}

/// The repo's constructs met but not read in `area`, by file and line.
pub fn in_area(conn: &Connection, area: &str) -> Result<Vec<Site>> {
    let mut stmt = conn.prepare_cached(
        "SELECT u.area, u.construct, f.path, u.line FROM unsupported u JOIN files f ON f.id = u.file_id
         WHERE u.area = ?1 ORDER BY f.path, u.line, u.rowid",
    )?;
    let rows = stmt.query_map([area], |r| Ok(Site { area: r.get(0)?, construct: r.get(1)?, file: r.get(2)?, line: r.get(3)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The repo's constructs met but not read in `file` between lines `from` and `to`, any area but `http` (an answer
/// reads those with the routes, by the symbol handling them).
pub fn in_span(conn: &Connection, file: &str, from: u32, to: u32) -> Result<Vec<Site>> {
    let mut stmt = conn.prepare_cached(
        "SELECT u.area, u.construct, f.path, u.line FROM unsupported u JOIN files f ON f.id = u.file_id
         WHERE f.path = ?1 AND u.line BETWEEN ?2 AND ?3 AND u.area != 'http' ORDER BY u.line, u.rowid",
    )?;
    let rows = stmt.query_map(rusqlite::params![file, from, to], |r| Ok(Site { area: r.get(0)?, construct: r.get(1)?, file: r.get(2)?, line: r.get(3)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every construct met but not read in the repo, any area: for counting.
pub fn all(conn: &Connection) -> Result<Vec<Site>> {
    let mut stmt = conn.prepare_cached(
        "SELECT u.area, u.construct, f.path, u.line FROM unsupported u JOIN files f ON f.id = u.file_id
         ORDER BY u.area, f.path, u.line, u.rowid",
    )?;
    let rows = stmt.query_map([], |r| Ok(Site { area: r.get(0)?, construct: r.get(1)?, file: r.get(2)?, line: r.get(3)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many sites an entry names; the rest are counted.
const NAMED: usize = 3;

/// One `unsupported` entry per area for `sites` (in `repo`, a name, when the answer crosses repos): how many, the
/// first few with file:line, and what is missing because of them (`missing`, by area).
pub fn incomplete(sites: &[Site], repo: Option<&str>) -> Vec<Incomplete> {
    let mut areas: Vec<&str> = sites.iter().map(|s| s.area.as_str()).collect();
    areas.sort_unstable();
    areas.dedup();
    let mut out = Vec::new();
    for area in areas {
        let of: Vec<&Site> = sites.iter().filter(|s| s.area == area).collect();
        let at = |s: &Site| match repo {
            Some(r) => format!("{} ({r}:{}:{})", s.construct, s.file, s.line),
            None => format!("{} ({}:{})", s.construct, s.file, s.line),
        };
        let mut named: Vec<String> = of.iter().take(NAMED).map(|s| at(s)).collect();
        if of.len() > NAMED {
            named.push(format!("{} more", of.len() - NAMED));
        }
        let what = if of.len() == 1 { "a construct KnowCode doesn't read" } else { "constructs KnowCode doesn't read" };
        out.push(
            Incomplete::new(Reason::Unsupported, format!("{area}: {what}: {}; {}", named.join("; "), missing(area)))
                .with_hint("read those lines: what they add isn't in this answer"),
        );
    }
    out
}

/// What is missing from an answer because of an area's constructs not read.
fn missing(area: &str) -> &'static str {
    match area {
        "di" => "registrations made through them aren't listed, and a service may be said unregistered",
        "bus" => "messages sent through them aren't linked to their consumers",
        "sql" => "the tables they name aren't listed",
        "ansible" => "the files they include or render aren't linked",
        "terraform" => "the addresses they name aren't counted as uses",
        "ci" => "jobs and includes they bring aren't in the pipeline",
        "angular" => "the routes they load aren't in the route table",
        "imports" => "what the project's files import that way isn't known: a reference to a type it imports may be ambiguous or unresolved",
        "http" => "the routes they register, and the HTTP callers of those routes, aren't linked",
        _ => "what they add isn't listed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_entry_per_area_naming_the_first_sites() {
        let site = |area: &str, line| Site { area: area.into(), construct: "scan".into(), file: "Startup.cs".into(), line };
        let sites = [site("di", 3), site("di", 4), site("di", 5), site("di", 6), site("sql", 9)];
        let got = incomplete(&sites, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].reason, Reason::Unsupported);
        assert!(got[0].detail.starts_with("di: constructs KnowCode doesn't read: scan (Startup.cs:3); scan (Startup.cs:4); scan (Startup.cs:5); 1 more; "), "{}", got[0].detail);
        assert!(got[1].detail.starts_with("sql: a construct KnowCode doesn't read: scan (Startup.cs:9); the tables"), "{}", got[1].detail);
        assert!(incomplete(&[site("bus", 1)], Some("orders"))[0].detail.contains("(orders:Startup.cs:1)"));
    }
}
