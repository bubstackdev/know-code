//! A repo's GitLab CI content as the workspace summary carries it (see `kc_extract::gitlabci`): its CI files, the
//! jobs they define and the facts pipelines are resolved from (`kc_index::ci`).

use anyhow::Result;
use rusqlite::Connection;

use crate::ansible::FactRow;

/// A job, hidden or not, and where it is defined.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobRow {
    pub name: String,
    pub file: String,
    pub line: u32,
}

/// What the workspace summary keeps of a repo's GitLab CI files.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Every CI file, by path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<JobRow>,
    /// `ci_*` facts in file and line order; a job's carry its name as `symbol`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<FactRow>,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The repo's GitLab CI summary.
pub fn summary(conn: &Connection) -> Result<Summary> {
    let files: Vec<String> = conn
        .prepare("SELECT path FROM files WHERE lang = 'gitlabci' ORDER BY path")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Summary { files, ..Summary::default() };
    if out.files.is_empty() {
        return Ok(out);
    }
    out.jobs = conn
        .prepare(
            "SELECT s.name, f.path, s.start_line FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'gitlabci' AND s.kind = 'job' ORDER BY f.path, s.start_line",
        )?
        .query_map([], |r| Ok(JobRow { name: r.get(0)?, file: r.get(1)?, line: r.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    out.facts = conn
        .prepare(
            "SELECT a.kind, a.name, a.value, f.path, a.line, s.name FROM ansible_facts a JOIN files f ON f.id = a.file_id
             LEFT JOIN symbols s ON s.id = a.symbol_id WHERE f.lang = 'gitlabci' ORDER BY f.path, a.line, a.rowid",
        )?
        .query_map([], |r| {
            Ok(FactRow { kind: r.get(0)?, name: r.get(1)?, value: r.get(2)?, file: r.get(3)?, line: r.get(4)?, symbol: r.get(5)?, role: None })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}
