export interface QueryRequest {
  op: 'symbols' | 'references' | 'source' | 'callers' | 'callees' | 'impact' | 'symbol_context' | 'coverage';
  name?: string;
  query?: string;
  symbol_id?: string;
  artifact_id?: string;
  target?: string;
  limit?: number;
  freshness_wait_ms?: number;
  deadline_ms?: number;
  kinds?: DependencyKind[];
  include_implicit?: boolean;
  exact_only?: boolean;
  max_depth?: number;
  max_edges?: number;
  max_nodes?: number;
  paths_per_symbol?: number;
  max_bytes?: number;
  time_budget_ms?: number;
  start_byte?: number;
  end_byte?: number;
  content_hash?: string;
  generation?: number;
  compilation_id?: string;
  include_source?: boolean;
  coverage_detail?: 'summary' | 'full';
  detail_max_bytes?: number;
}
export type DependencyKind = 'call' | 'reference' | 'type_reference' | 'inherits' | 'imports' | 'implements' | 'overrides' | 'contains' | 'initializes';
export interface DefinitionLink {
  reference_symbol_id: string; source_compilation_id: string;
  definition_symbol_id: string; definition_compilation_id: string; input_fingerprint: string;
}
export interface DispatchProof {
  dispatch_key?: string; source_id: string; target_id: string; kind: DependencyKind; compilation_id: string;
  location: Location; source_definition_link?: DefinitionLink; target_definition_link?: DefinitionLink;
}
export interface GraphEdge {
  dispatch_context_id?: string; dispatch_context_fingerprint?: string; dispatch_proof?: DispatchProof; binding_definition_link?: DefinitionLink;
  target_compilation_id?: string; definition_link?: DefinitionLink;
  dispatch_key?: string; source_id: string; target_id: string | null; kind: DependencyKind;
  resolution: 'compiler_bound' | 'possible_dispatch' | 'unresolved'; implicit: boolean;
  dispatch: '' | 'direct' | 'virtual' | 'interface' | 'indirect';
  compilation_id: string; target: string; location: Location;
  binding_target_id?: string; dispatch_evidence?: Location;
}
export interface GraphResult {
  root_definitions?: DefinitionLink[];
  findings: Array<{symbol: SymbolResult; paths: GraphEdge[][]}>;
  boundaries: Array<{reason: string; symbol_id: string; compilation_id: string; evidence?: GraphEdge; context_conflict?: {project_path: string; selected_compilation_id: string; required_compilation_id: string}} >;
  capability_coverage: Record<string, 'complete' | 'partial' | 'unavailable' | 'unknown'>;
  limits: string[]; examined_edges: number; visited_nodes: number;
}
export interface Location {
  artifact_id: string; path: string; content_hash: string;
  start_byte: number; end_byte: number; generated: boolean;
}
export interface SymbolResult {
  id: string; name: string; kind: string; signature: string; external: boolean;
  compilation_id: string; target: string; locations: Location[];
}
export interface QueryResult {
  generation: number; freshness: 'current' | 'stale' | 'unverified'; truncated: boolean;
  coordination?: WorkspaceStatus;
  workspace_coverage: {compilations: number; statuses: Record<string, number>; diagnostic_counts: Record<string, number>};
  coverage_scope: 'workspace' | 'relevant' | 'compilation';
  contexts?: Array<{symbol: SymbolResult; boundary?: string; declarations: Array<{
    location: Location; line: number; extent?: Location; source?: SourceResult;
    source_status: 'included' | 'truncated' | 'omitted' | 'extent_unavailable' | 'extent_ambiguous' | 'budget_exhausted';
  }>}>;
  coverage: Array<{compilation_id: string; language: string; project_path: string;
    target: string; configuration: string; inferred: boolean; compiler_version: string; analysis_options?: Record<string, unknown>;
    status: 'complete' | 'partial' | 'unavailable'; capabilities: Record<string, string>;
    details_omitted: boolean; diagnostic_counts: Record<string, number>;
    diagnostics?: Array<{code: string; severity: string; message: string; path: string}>}>;
  symbols?: SymbolResult[];
  occurrences?: Array<{symbol_id: string; role: string; compilation_id: string; target: string; location: Location}>;
  source?: SourceResult;
  graph?: GraphResult;
}
export type QueryResponse = {result: QueryResult} | {error: {code: string; message: string}};
export class Index {
  constructor(database: string);
  query(request: QueryRequest): Promise<QueryResponse>;
}

export interface SourceResult {artifact_id: string; path: string; content_hash: string; text: string; start_byte: number; end_byte: number; total_bytes: number; start_line: number}

export interface WorkspaceStatus {
  workspace_root: string; database: string; phase: string;
  observed_epoch: number; published_epoch: number; generation: number;
  available: boolean; error: string; outcome_code: string;
  progress: {stage: string; project_path: string; message: string} | null;
}
export interface WorkspaceOptions {configuration?: string; dataDirectory?: string; coordinatorExecutable?: string}
export class KnowCodeError extends Error {readonly code: string; constructor(code: string, message: string)}
export class Workspace {
  private constructor();
  static open(root: string, options?: WorkspaceOptions): Promise<Workspace>;
  readonly root: string;
  readonly initialStatus: Readonly<WorkspaceStatus>;
  status(): Promise<WorkspaceStatus>;
  cancelIndexing(): Promise<WorkspaceStatus>;
  refresh(options?: {configuration?: string}): Promise<WorkspaceStatus>;
  waitForIndex(options?: {waitMs?: number}): Promise<WorkspaceStatus>;
  query(request: QueryRequest): Promise<{result: QueryResult}>;
  close(): void;
}
