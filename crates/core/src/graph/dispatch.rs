//! Compiler-proven dispatch facts, streamed under the parent graph's budgets.
//! Ancestry semantics belong to adapters; this module never guesses by name.
use super::*;

pub(super) fn create_view(connection: &Connection) -> Result<()> {
    // Explicit branches let either endpoint restriction reach a durable index.
    // COALESCE over linked endpoints would force a full relationship scan.
    let mut branches = vec![];
    for source_linked in [false, true] {
        for target_linked in [false, true] {
            let mut joins = String::new();
            let mut conditions = String::new();
            for (linked, side, alias) in [
                (source_linked, "source", "ds"),
                (target_linked, "target", "dt"),
            ] {
                if linked {
                    joins += &format!(
                        " JOIN definition_links {alias} ON {alias}.source_key=r.{side}_key"
                    );
                } else {
                    conditions += &format!(
                        " AND NOT EXISTS(SELECT 1 FROM definition_links {alias} WHERE {alias}.source_key=r.{side}_key)"
                    );
                }
            }
            let source = if source_linked {
                "ds.target_key"
            } else {
                "r.source_key"
            };
            let target = if target_linked {
                "dt.target_key"
            } else {
                "r.target_key"
            };
            let sf = if source_linked {
                "ds.fingerprint"
            } else {
                "NULL"
            };
            let tf = if target_linked {
                "dt.fingerprint"
            } else {
                "NULL"
            };
            branches.push(format!("SELECT {source} AS source_key,{target} AS target_key,r.source_key AS source_reference_key,r.target_key AS target_reference_key,{sf} AS source_fingerprint,{tf} AS target_fingerprint,r.kind,r.artifact_key,r.start_byte,r.end_byte,r.dispatch_key FROM relationships r{joins} WHERE r.kind IN('implements','overrides') AND r.resolution='compiler_bound'{conditions}"));
        }
    }
    connection.execute_batch(&format!(
        "CREATE TEMP VIEW dispatch_implementations AS {}",
        branches.join(" UNION ALL ")
    ))?;
    Ok(())
}
pub(super) fn proof_sql(incoming: bool) -> String {
    // incoming query: find the slot(s) implemented by this body. Outgoing query:
    // find the body(s) implementing the call's compiler-resolved slot.
    let column = if incoming {
        "p.source_key"
    } else {
        "p.target_key"
    };
    format!("SELECT p.source_key,p.target_key,s.id,s.compilation_id,rs.id,rs.compilation_id,rt.id,p.kind,
      a.id,a.path,a.content_hash,p.start_byte,p.end_byte,a.generated,p.source_fingerprint,p.target_fingerprint,t.id,t.compilation_id,p.dispatch_key
      FROM dispatch_implementations p CROSS JOIN symbols s ON s.rowid=p.source_key
      CROSS JOIN symbols t ON t.rowid=p.target_key CROSS JOIN symbols rs ON rs.rowid=p.source_reference_key
      CROSS JOIN symbols rt ON rt.rowid=p.target_reference_key CROSS JOIN artifacts a ON a.rowid=p.artifact_key
      CROSS JOIN generation_units u ON u.owner_generation=rs.generation AND u.compilation_id=rs.compilation_id
      WHERE u.generation=? AND {column}=? LIMIT ?")
}
struct Proof {
    source: i64,
    target: i64,
    id: String,
    compilation: String,
    evidence: DispatchProof,
}
fn proof(row: &rusqlite::Row<'_>) -> rusqlite::Result<Proof> {
    let id: String = row.get(2)?;
    let compilation: String = row.get(3)?;
    let owner: String = row.get(5)?;
    let source_id: String = row.get(4)?;
    let target_id: String = row.get(6)?;
    Ok(Proof {
        source: row.get(0)?,
        target: row.get(1)?,
        id: id.clone(),
        compilation: compilation.clone(),
        evidence: DispatchProof {
            dispatch_key: row.get(18)?,
            source_id: source_id.clone(),
            target_id: target_id.clone(),
            compilation_id: owner.clone(),
            kind: DependencyKind::parse(&row.get::<_, String>(7)?)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            location: location(row, 8)?,
            source_definition_link: row.get::<_, Option<String>>(14)?.map(|input_fingerprint| {
                DefinitionLink {
                    reference_symbol_id: source_id,
                    source_compilation_id: owner.clone(),
                    definition_symbol_id: id,
                    definition_compilation_id: compilation,
                    input_fingerprint,
                }
            }),
            target_definition_link: if let Some(input_fingerprint) =
                row.get::<_, Option<String>>(15)?
            {
                Some(DefinitionLink {
                    reference_symbol_id: target_id,
                    source_compilation_id: owner,
                    definition_symbol_id: row.get(16)?,
                    definition_compilation_id: row.get(17)?,
                    input_fingerprint,
                })
            } else {
                None
            },
        },
    })
}
pub(super) struct Budget {
    pub edges: u32,
    pub started: Instant,
    pub duration: Duration,
}
impl Budget {
    fn remaining(&self, result: &GraphResult) -> u32 {
        self.edges.saturating_sub(result.examined_edges)
    }
    fn take(&self, result: &mut GraphResult) -> bool {
        if self.started.elapsed() >= self.duration {
            result.limits.push("time_budget".into());
            false
        } else if self.remaining(result) == 0 {
            result.limits.push("edge_budget".into());
            false
        } else {
            result.examined_edges += 1;
            true
        }
    }
}
fn accepted(edge: &Edge, request: &QueryRequest) -> bool {
    (request.op == "impact" || edge.kind == DependencyKind::Call)
        && request
            .kinds
            .as_ref()
            .is_none_or(|k| k.contains(&edge.kind))
        && (request.include_implicit || !edge.implicit)
        && (!request.exact_only || edge.resolution != "possible_dispatch")
}
fn candidate(call: &Neighbor, implementation: &Proof) -> Neighbor {
    let mut edge = call.edge.clone();
    edge.binding_target_id = edge.binding_target_id.or_else(|| edge.target_id.clone());
    edge.binding_definition_link = edge.definition_link;
    edge.definition_link = implementation
        .evidence
        .source_definition_link
        .clone()
        .map(Box::new);
    edge.target_id = Some(implementation.id.clone());
    edge.target_compilation_id = Some(implementation.compilation.clone());
    edge.resolution = "possible_dispatch".into();
    edge.dispatch_evidence = Some(Box::new(implementation.evidence.location.clone()));
    edge.dispatch_proof = Some(Box::new(implementation.evidence.clone()));
    Neighbor {
        source: call.source,
        target: Some(implementation.source),
        edge,
    }
}
pub(super) fn neighbors(
    view: &ReadView,
    node: i64,
    incoming: bool,
    request: &QueryRequest,
    budget: &Budget,
    result: &mut GraphResult,
) -> Result<Vec<Neighbor>> {
    let mut output = vec![];
    let mut stmt = view.connection.prepare_cached(&adjacency_sql(
        incoming,
        false,
        view.format_version >= 6,
    ))?;
    let mut rows = stmt.query(params![view.generation, node, budget.remaining(result) + 1])?;
    while let Some(row) = rows.next()? {
        view.check()?;
        if !budget.take(result) {
            break;
        }
        let neighbor = read_neighbor(row)?;
        if accepted(&neighbor.edge, request) {
            output.push(neighbor);
        }
    }
    drop(rows);
    if view.format_version < 6
        || request.exact_only
        || request
            .kinds
            .as_ref()
            .is_some_and(|k| !k.contains(&DependencyKind::Call))
    {
        return Ok(output);
    }
    if incoming {
        let mut implementations = view.connection.prepare_cached(&proof_sql(true))?;
        let mut rows =
            implementations.query(params![view.generation, node, budget.remaining(result) + 1])?;
        while let Some(row) = rows.next()? {
            view.check()?;
            if !budget.take(result) {
                break;
            }
            let implementation = proof(row)?;
            let mut calls = view
                .connection
                .prepare_cached(&adjacency_sql(true, true, true))?;
            let mut call_rows = calls.query(params![
                view.generation,
                implementation.target,
                budget.remaining(result) + 1
            ])?;
            while let Some(row) = call_rows.next()? {
                view.check()?;
                if !budget.take(result) {
                    break;
                }
                let call = read_neighbor(row)?;
                if accepted(&call.edge, request)
                    && !call.edge.dispatch_key.is_empty()
                    && call.edge.dispatch_key == implementation.evidence.dispatch_key
                {
                    output.push(candidate(&call, &implementation));
                }
            }
        }
    } else {
        let mut candidates = vec![];
        for call in &output {
            view.check()?;
            if call.edge.kind != DependencyKind::Call
                || call.edge.resolution != "compiler_bound"
                || !matches!(call.edge.dispatch.as_str(), "virtual" | "interface")
            {
                continue;
            }
            if call.edge.dispatch_key.is_empty() {
                continue;
            }
            let Some(slot) = call.target else { continue };
            let mut implementations = view.connection.prepare_cached(&proof_sql(false))?;
            let mut rows = implementations.query(params![
                view.generation,
                slot,
                budget.remaining(result) + 1
            ])?;
            while let Some(row) = rows.next()? {
                view.check()?;
                if !budget.take(result) {
                    break;
                }
                let implementation = proof(row)?;
                if implementation.source != slot
                    && !call.edge.dispatch_key.is_empty()
                    && call.edge.dispatch_key == implementation.evidence.dispatch_key
                {
                    candidates.push(candidate(call, &implementation));
                }
            }
        }
        output.extend(candidates);
    }
    Ok(output)
}
