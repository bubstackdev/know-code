//! Shared, evidence-based test classification. No language heuristics live here.
use crate::{
    Error, Result,
    protocol::{TestClassification, TestEvidence},
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub const POLICY_VERSION: u32 = 1;
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestFilter {
    #[default]
    All,
    Exclude,
    Only,
}
impl TestFilter {
    pub(crate) fn predicate(self, mask: &str) -> String {
        match self {
            Self::All => "1".into(),
            Self::Only => format!("(({mask}) & 2)<>0"),
            Self::Exclude => format!("(({mask}) & 5)<>0"),
        }
    }
    pub(crate) fn matches(self, mask: u8) -> bool {
        match self {
            Self::All => true,
            Self::Only => mask & 2 != 0,
            Self::Exclude => mask & 5 != 0,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Classification {
    pub kind: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<OriginEvidence>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub evidence_limited: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct OriginEvidence {
    pub kind: String,
    pub artifact_id: String,
    pub source_path: String,
    pub evidence: Vec<TestEvidence>,
}
pub(crate) fn encode(class: Option<&TestClassification>) -> Result<String> {
    let Some(class) = class else {
        return Ok(String::new());
    };
    if class.policy_version != POLICY_VERSION
        || !["unknown", "test", "non_test"].contains(&class.kind.as_str())
        || (class.kind != "unknown" && class.evidence.is_empty())
        || class.evidence.len() > 8
        || class.evidence.iter().any(|e| {
            e.basis.is_empty() || e.basis.len() > 64 || e.value.len() > 512 || e.path.len() > 4096
        })
    {
        return Err(Error::Protocol(
            "invalid test classification or evidence".into(),
        ));
    }
    Ok(serde_json::to_string(class)?)
}
// JSON is validated once when written. Missing legacy metadata remains unknown.
pub(crate) fn create_views(connection: &Connection, version: i32) -> Result<()> {
    let class = if version >= 7 {
        "CASE WHEN a.test_classification_json<>'' THEN a.test_classification_json ELSE c.test_classification_json END"
    } else {
        "''"
    };
    let mask = if version >= 7 { "test_mask" } else { "1" };
    let symbols = if version >= 7 {
        "SELECT * FROM symbols"
    } else {
        "SELECT *,1 AS test_mask FROM symbols"
    };
    connection.execute_batch(&format!("CREATE TEMP VIEW test_origins AS SELECT a.owner_generation,a.compilation_id,a.artifact_id,{class} AS classification_json FROM unit_artifacts a JOIN compilations c ON c.generation=a.owner_generation AND c.id=a.compilation_id;
      CREATE TEMP VIEW test_origin_masks AS SELECT *, CASE json_extract(nullif(classification_json,''),'$.kind') WHEN 'test' THEN 2 WHEN 'non_test' THEN 4 ELSE 1 END AS mask FROM test_origins;
      CREATE TEMP VIEW symbol_test_masks AS SELECT rowid AS symbol_key,{mask} AS mask FROM symbols; CREATE TEMP VIEW classified_symbols AS {symbols};"))?;
    Ok(())
}
fn kind(mask: u8) -> &'static str {
    match mask {
        2 => "test",
        4 => "non_test",
        3 | 6 | 7 => "mixed",
        _ => "unknown",
    }
}
pub(crate) fn symbol_mask(connection: &Connection, key: i64) -> Result<u8> {
    Ok(connection
        .prepare_cached("SELECT mask FROM symbol_test_masks WHERE symbol_key=?")?
        .query_row([key], |r| r.get(0))?)
}
pub(crate) fn symbol(connection: &Connection, key: i64) -> Result<Classification> {
    let mask = symbol_mask(connection, key)?;
    let external: bool = connection
        .prepare_cached("SELECT external FROM symbols WHERE rowid=?")?
        .query_row([key], |r| r.get(0))?;
    let mut result = Classification {
        kind: kind(mask).into(),
        ..Default::default()
    };
    if external {
        return Ok(result);
    }
    let mut q = connection.prepare_cached("SELECT id,path,classification_json FROM (SELECT DISTINCT a.id,a.path,t.classification_json,t.mask FROM symbols s JOIN occurrence_locations o ON o.generation=s.generation AND o.compilation_id=s.compilation_id AND o.symbol_id=s.id JOIN artifacts a ON a.generation=o.generation AND a.id=o.artifact_id JOIN test_origin_masks t ON t.owner_generation=s.generation AND t.compilation_id=s.compilation_id AND t.artifact_id=a.id WHERE s.rowid=? AND o.role='declaration') ORDER BY row_number() OVER (PARTITION BY mask ORDER BY path,id),path,id LIMIT 9")?;
    for row in q.query_map([key], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (id, path, json) = row?;
        if result.evidence.len() == 8 {
            result.evidence_limited = true;
            break;
        }
        result.evidence.push(origin_evidence(id, path, &json)?);
    }
    Ok(result)
}
fn origin_evidence(artifact_id: String, source_path: String, json: &str) -> Result<OriginEvidence> {
    let class = if json.is_empty() {
        TestClassification {
            kind: "unknown".into(),
            ..Default::default()
        }
    } else {
        serde_json::from_str(json)?
    };
    Ok(OriginEvidence {
        kind: class.kind,
        artifact_id,
        source_path,
        evidence: class.evidence,
    })
}
pub(crate) fn origin(artifact: &str, path: &str, json: &str) -> Result<Classification> {
    let evidence = origin_evidence(artifact.into(), path.into(), json)?;
    Ok(Classification {
        kind: evidence.kind.clone(),
        evidence: vec![evidence],
        evidence_limited: false,
    })
}
pub(crate) fn finalize(connection: &Connection, generation: u64, compilation: &str) -> Result<()> {
    connection.execute("UPDATE symbols AS s SET test_mask=CASE WHEN external THEN 1 ELSE coalesce((SELECT sum(DISTINCT t.mask) FROM occurrences o JOIN artifacts a ON a.rowid=o.artifact_key JOIN test_origin_masks t ON t.owner_generation=s.generation AND t.compilation_id=s.compilation_id AND t.artifact_id=a.id WHERE o.symbol_key=s.rowid AND o.role='declaration'),1) END WHERE generation=? AND compilation_id=?",params![generation,compilation])?;
    Ok(())
}
