// IndexStore relation roles are compiler evidence, not syntax/name matches.
// Keep extraction here; storage, dispatch expansion and traversal stay in Rust.
const roles = {declaration: 3, reference: 4, call: 32, dynamic: 64, implicit: 256,
  childOf: 512, baseOf: 1024, overrideOf: 2048, calledBy: 8192, accessorOf: 32768, containedBy: 65536};
const callables = new Set([12, 16, 17, 18, 22, 23, 24]);
const types = new Set([5, 6, 7, 8, 9, 10, 11]);
const implicitCalls = new Set([3, 4, 1000, 1001, 1002, 1003, 1008, 1009, 1010, 1014, 1015, 1016, 1017, 1018]);
export const graphCapabilities = {calls: 'partial', dependencies: 'partial', dispatch: 'partial', project_links: 'unavailable'};
export const unavailableGraphCapabilities = {calls: 'unavailable', dependencies: 'unavailable', dispatch: 'unavailable', project_links: 'unavailable'};

export function createGraph({id, symbols, emit}) {
  let seen = new Set();
  const omitted = {callOwner: 0, callRange: 0, dependencyOwner: 0};
  const key = usr => `${id}:${usr}`;
  function relationship(source, target, kind, sourceArtifact, range, implicit = false, dispatch = '') {
    if (!source?.usr || !target?.usr || !range) return;
    const sourceId = key(source.usr), targetId = key(target.usr);
    if (!symbols.has(sourceId) || !symbols.has(targetId)) return;
    const row = {sourceId, targetId, kind, resolution: 'compiler_bound', artifactId: sourceArtifact.id,
      startByte: range[0], endByte: range[1], implicit, dispatch};
    const identity = JSON.stringify(row);
    if (seen.has(identity)) return;
    seen.add(identity); emit(row);
  }
  return {
    reset() { seen = new Set(); },
    occurrence(item, source, range) {
      const relations = item.relations.filter(relation => relation.usr);
      const implicit = Boolean(item.roles & roles.implicit);
      if (item.roles & roles.declaration) {
        for (const parent of relations.filter(r => r.roles & (roles.childOf | roles.accessorOf)))
          relationship(parent, item, 'contains', source, range, implicit);
      }
      for (const related of relations) {
        if (related.roles & roles.baseOf)
          relationship(related, item, item.kind === 8 && related.kind !== 8 ? 'implements' : 'inherits', source, range, implicit);
        if (related.roles & roles.overrideOf)
          relationship(item, related, 'overrides', source, range, implicit);
      }
      if (item.roles & roles.call) {
        // CALLEDBY, unlike CONTAINEDBY, asserts an invocation's execution owner.
        // A property/global/closure without a proven callable owner remains an
        // explicit gap; never attribute it to later value use or file loading.
        const callers = relations.filter(r => r.roles & roles.calledBy);
        const supported = callers.filter(r => callables.has(r.kind));
        if (!supported.length) {
          omitted.callOwner++;
          // Static dependency is still proven by the reference and lexical
          // containment. This does not assert when a closure/initializer runs.
          if (item.roles & roles.reference)
            for (const owner of relations.filter(r => r.roles & roles.containedBy))
              relationship(owner, item, 'reference', source, range, implicit);
        }
        else if (!range) omitted.callRange++;
        for (const caller of supported)
          relationship(caller, item, 'call', source, range,
            implicit || implicitCalls.has(item.subkind) || item.kind === 24,
            item.roles & roles.dynamic ? 'virtual' : 'direct');
        return;
      }
      if (!(item.roles & roles.reference) || !range) return;
      // The compiler's lexical owner is enough for a dependency, not a call.
      const owners = relations.filter(r => r.roles & roles.containedBy);
      if (!owners.length) omitted.dependencyOwner++;
      for (const owner of owners)
        relationship(owner, item, types.has(item.kind) ? 'type_reference' : item.kind === 1 ? 'imports' : 'reference', source, range, implicit);
    },
    diagnostics() {
      const diagnostics = [{code: 'swift_graph_partial', severity: 'info', path: '',
        message: 'Swift graph extraction covers compiler CALLEDBY calls and source-backed containment, value/type dependencies and ancestry. '
          + 'Unproven initializer/closure owners, implicit sites without source anchors, complete import coverage, generic dispatch and cross-project bodies remain partial.'}];
      if (Object.values(omitted).some(Boolean)) diagnostics.push({code: 'swift_graph_evidence_unavailable', severity: 'warning', path: '',
        message: `${omitted.callOwner} callsites lacked a supported compiler execution owner; ${omitted.callRange} calls lacked a usable source range; `
          + `${omitted.dependencyOwner} non-call references lacked compiler containment. Compiler containment dependencies are retained; no execution owners or calls were inferred.`});
      return diagnostics;
    }
  };
}
