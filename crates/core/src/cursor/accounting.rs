//! Private logical retention accounting; not SQLite physical file attribution.
use super::*;
use crate::execution::{Execution, QueryStart};
use serde::Serialize;

pub const ACCOUNTING_VERSION: u32 = 1;
const ROW_OVERHEAD: u64 = 64;
// Keep ownership lookups in unit -> link -> artifact order. Reordering from
// artifacts scanned the owner's links repeatedly (measured on frozen Polly).
const SETS: &str = r#"WITH
baseline_units AS MATERIALIZED (
 SELECT DISTINCT owner_generation,compilation_id FROM generation_units
 WHERE generation=?1 OR generation IN(SELECT generation FROM staging_generations)),
pinned_generations AS MATERIALIZED (
 SELECT DISTINCT generation FROM cursor_generation_pins WHERE owner=?2),
retained_units AS MATERIALIZED (
 SELECT DISTINCT u.owner_generation,u.compilation_id FROM generation_units u
 JOIN pinned_generations p ON p.generation=u.generation
 EXCEPT SELECT * FROM baseline_units),
baseline_artifacts AS MATERIALIZED (
 SELECT DISTINCT a.rowid,a.content_hash FROM baseline_units b
 CROSS JOIN unit_artifacts u USING(owner_generation,compilation_id)
 CROSS JOIN artifacts a ON a.generation=u.owner_generation AND a.id=u.artifact_id),
retained_artifacts AS MATERIALIZED (
 SELECT DISTINCT a.rowid,a.content_hash FROM retained_units r
 CROSS JOIN unit_artifacts u USING(owner_generation,compilation_id)
 CROSS JOIN artifacts a ON a.generation=u.owner_generation AND a.id=u.artifact_id
 EXCEPT SELECT * FROM baseline_artifacts),
retained_contents AS MATERIALIZED (
 SELECT DISTINCT content_hash FROM retained_artifacts
 EXCEPT SELECT content_hash FROM baseline_artifacts)
"#;

/// Version 1 policy charges: UTF-8/blob bytes, eight bytes per number, one per
/// NULL, plus 64 bytes per stored row. SQLite indexes, allocation/freelist/WAL
/// sizes are separate physical measurements, not promised by this metric.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RetainedUsage {
    pub metric_version: u32,
    pub serving_generation: u64,
    pub proposed_generation: u64,
    pub units: u64,
    pub unit_bytes: u64,
    pub artifact_bytes: u64,
    pub source_bytes: u64,
    pub manifest_bytes: u64,
    pub pin_bytes: u64,
}
impl RetainedUsage {
    pub fn total_bytes(&self) -> Result<u64> {
        [
            self.unit_bytes,
            self.artifact_bytes,
            self.source_bytes,
            self.manifest_bytes,
            self.pin_bytes,
        ]
        .into_iter()
        .try_fold(0u64, add)
    }
}
pub(super) fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| Error::ResourceLimited("cursor retention accounting overflow".into()))
}

// Only private, fixed schema tables/aliases enter these statements. Inspecting
// columns automatically charges new stored evidence rather than omitting it.
fn row_cost(connection: &Connection, table: &str) -> Result<String> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |r| r.get::<_, String>(1))?;
    let mut expression = ROW_OVERHEAD.to_string();
    for name in names {
        let name = name?.replace('"', "\"\"");
        expression.push_str(&format!("+CASE typeof(x.\"{name}\") WHEN 'null' THEN 1 WHEN 'integer' THEN 8 WHEN 'real' THEN 8 ELSE length(CAST(x.\"{name}\" AS BLOB)) END"));
    }
    Ok(expression)
}

fn unit_queries(connection: &Connection) -> Result<Vec<String>> {
    [
        ("compilations", "WHERE x.generation=?1 AND x.id=?2"),
        ("symbols", "WHERE x.generation=?1 AND x.compilation_id=?2"),
        (
            "occurrences",
            "JOIN symbols s ON s.rowid=x.symbol_key WHERE s.generation=?1 AND s.compilation_id=?2",
        ),
        (
            "relationships",
            "JOIN symbols s ON s.rowid=x.source_key WHERE s.generation=?1 AND s.compilation_id=?2",
        ),
        ("reports", "WHERE x.generation=?1 AND x.compilation_id=?2"),
        (
            "unit_artifacts",
            "WHERE x.owner_generation=?1 AND x.compilation_id=?2",
        ),
    ]
    .into_iter()
    .map(|(table, selector)| {
        Ok(format!(
            "SELECT coalesce(sum({}),0) FROM {table} x {selector}",
            row_cost(connection, table)?
        ))
    })
    .collect()
}

impl GenerationRetention {
    /// Measure one coherent snapshot. A proposed generation must be the
    /// current generation or an owned active stage, not historical selection.
    /// Call under the same coordinator admission/publication lock as pins.
    pub fn retained_usage(
        &self,
        proposed: Option<u64>,
        deadline_ms: Option<u32>,
    ) -> Result<RetainedUsage> {
        self.usage_controlled(
            proposed,
            Execution::new(QueryStart::default(), deadline_ms)?,
        )
    }
    pub(super) fn usage_controlled(
        &self,
        proposed: Option<u64>,
        execution: Execution,
    ) -> Result<RetainedUsage> {
        execution.check()?;
        let result = self.measure(proposed, execution.clone());
        execution.check()?;
        result
    }
    fn open_accounting(&self, execution: Execution) -> Result<Connection> {
        let connection = open_writer_controlled(&self.database, Some(execution))?;
        let version: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != crate::store::SCHEMA_VERSION {
            return Err(Error::UpdateRequired(
                "refresh this workspace to upgrade its index before retaining paged results".into(),
            ));
        }
        Ok(connection)
    }
    fn remember_unit(
        &self,
        connection: &mut Connection,
        owner: u64,
        id: &str,
        bytes: u64,
        execution: Execution,
    ) -> Result<()> {
        execution.check()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.check_owner(&tx)?;
        tx.execute("INSERT INTO cursor_unit_costs_v1 SELECT generation,id,?3,?4 FROM compilations WHERE generation=?1 AND id=?2 AND generation NOT IN(SELECT generation FROM staging_generations) ON CONFLICT(owner_generation,compilation_id) DO UPDATE SET bytes=excluded.bytes,store_format=excluded.store_format", params![owner, id, bytes, crate::store::SCHEMA_VERSION])?;
        execution.check()?;
        tx.commit()?;
        Ok(())
    }
    fn measure(&self, proposed: Option<u64>, execution: Execution) -> Result<RetainedUsage> {
        let mut connection = self.open_accounting(execution.clone())?;
        // Immutable facts are scanned in a read snapshot. A second connection
        // commits each completed summary in a short IMMEDIATE transaction, so
        // interruption preserves progress without upgrading a read transaction.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        self.check_owner(&tx)?;
        let serving: Option<u64> =
            tx.query_row("SELECT current_generation FROM state", [], |r| r.get(0))?;
        let serving = serving.ok_or(Error::NotReady)?;
        let proposed = proposed.unwrap_or(serving);
        if proposed != serving
            && !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM staging_generations WHERE generation=?)",
                [proposed],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err(Error::InvalidRequest(
                "proposed publication is not an active staging generation".into(),
            ));
        }
        let mut usage = RetainedUsage {
            metric_version: ACCOUNTING_VERSION,
            serving_generation: serving,
            proposed_generation: proposed,
            ..Default::default()
        };
        let mut cache_writer = None;
        {
            let mut statement = tx.prepare(&format!("{SETS} SELECT r.owner_generation,r.compilation_id,c.bytes FROM retained_units r LEFT JOIN cursor_unit_costs_v1 c ON c.owner_generation=r.owner_generation AND c.compilation_id=r.compilation_id AND c.store_format=?3"))?;
            let rows = statement.query_map(
                params![
                    proposed,
                    self.owner.as_slice(),
                    crate::store::SCHEMA_VERSION
                ],
                |r| {
                    Ok((
                        r.get::<_, u64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<u64>>(2)?,
                    ))
                },
            )?;
            let mut queries = None;
            for row in rows {
                execution.check()?;
                let (owner, id, cached) = row?;
                let bytes = if let Some(bytes) = cached {
                    bytes
                } else {
                    if queries.is_none() {
                        queries = Some(unit_queries(&tx)?);
                    }
                    // Charge the summary's own row as well as the facts.
                    let mut bytes = add(ROW_OVERHEAD + 24, id.len() as u64)?;
                    for sql in queries.as_ref().expect("initialized unit queries") {
                        bytes = add(
                            bytes,
                            tx.prepare_cached(sql)?
                                .query_row(params![owner, id], |r| r.get(0))?,
                        )?;
                    }
                    if cache_writer.is_none() {
                        cache_writer = Some(self.open_accounting(execution.clone())?);
                    }
                    self.remember_unit(
                        cache_writer.as_mut().expect("initialized cache writer"),
                        owner,
                        &id,
                        bytes,
                        execution.clone(),
                    )?;
                    bytes
                };
                usage.units += 1;
                usage.unit_bytes = add(usage.unit_bytes, bytes)?;
            }
        }
        for (field, table, selector) in [
            (
                &mut usage.artifact_bytes,
                "artifacts",
                "WHERE x.rowid IN(SELECT rowid FROM retained_artifacts)",
            ),
            (
                &mut usage.source_bytes,
                "contents",
                "WHERE x.hash IN(SELECT content_hash FROM retained_contents)",
            ),
            (
                &mut usage.manifest_bytes,
                "generation_units",
                "WHERE x.generation IN(SELECT generation FROM pinned_generations) AND x.generation<>?1 AND x.generation NOT IN(SELECT generation FROM staging_generations)",
            ),
        ] {
            let sql = format!(
                "{SETS} SELECT coalesce(sum({}),0) FROM {table} x {selector}",
                row_cost(&tx, table)?
            );
            *field = tx.query_row(&sql, params![proposed, self.owner.as_slice()], |r| r.get(0))?;
        }
        let generations: u64 = tx.query_row(&format!("{SETS} SELECT count(*) FROM pinned_generations WHERE generation<>?1 AND generation NOT IN(SELECT generation FROM staging_generations)"), params![proposed, self.owner.as_slice()], |r| r.get(0))?;
        usage.manifest_bytes = add(
            usage.manifest_bytes,
            generations.checked_mul(ROW_OVERHEAD + 8).ok_or_else(|| {
                Error::ResourceLimited("cursor generation accounting overflow".into())
            })?,
        )?;
        usage.pin_bytes = tx.query_row(
            &format!(
                "SELECT coalesce(sum({}),0) FROM cursor_generation_pins x WHERE owner=?",
                row_cost(&tx, "cursor_generation_pins")?
            ),
            [self.owner.as_slice()],
            |r| r.get(0),
        )?;
        tx.commit()?;
        usage.total_bytes()?;
        Ok(usage)
    }
}
