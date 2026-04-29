export type ReindexPhase = 'start' | 'indexing' | 'complete';

export type DashboardEvent =
	| { type: 'ping'; data: { ts_ms: number } }
	| { type: 'file_changed'; data: { paths: string[] } }
	| { type: 'index_updated'; data: { changed: number; deleted: number } }
	| { type: 'reindex_progress'; data: { phase: ReindexPhase; percent: number } };

export interface ProjectSummary {
	root: string;
	files: number;
	symbols: number;
	indexed: boolean;
}

export interface FocusedFileSymbol {
	name: string;
	kind: string;
	line_start: number;
	line_end: number;
	visibility: 'public' | 'private';
	complexity: number | null;
}

export interface FocusedFileDependent {
	path: string;
	kind: string;
}

export interface FocusedFileImpact {
	direct: number;
	transitive: number;
}

export interface FocusedFile {
	path: string;
	language: string;
	lines: number;
	symbols: FocusedFileSymbol[];
	dependents: FocusedFileDependent[];
	impact: FocusedFileImpact;
}

export interface GraphNode {
	id: number;
	path: string;
	language: string;
	pagerank: number;
	loc: number;
	hot_score: number;
	cluster_id: number | null;
}

export interface GraphLink {
	source: number;
	target: number;
	kind: string;
}

export interface CoChangeLink {
	source: number;
	target: number;
	count: number;
}

export interface GraphResponse {
	nodes: GraphNode[];
	links: GraphLink[];
	cochanges: CoChangeLink[];
	truncated: boolean;
}

export interface SymbolGraphNode {
	id: number;
	name: string;
	kind: string;
	file_id: number;
	file_path: string;
	pagerank: number;
	complexity: number | null;
}

export interface SymbolGraphLink {
	source: number;
	target: number;
	kind: string;
}

export interface SymbolGraphResponse {
	nodes: SymbolGraphNode[];
	links: SymbolGraphLink[];
	truncated: boolean;
}

export interface FocusedSymbolNeighbor {
	id: number;
	name: string;
	kind: string;
	file_path: string;
	line_start: number;
}

export interface FocusedSymbol {
	id: number;
	name: string;
	kind: string;
	signature: string | null;
	file_path: string;
	line_start: number;
	line_end: number;
	complexity: number | null;
	callers: FocusedSymbolNeighbor[];
	callees: FocusedSymbolNeighbor[];
	reference_count: number;
}

export interface SymbolSearchMatch {
	id: number;
	name: string;
	kind: string;
	file_path: string;
	line_start: number;
}

export interface SymbolSearchResponse {
	matches: SymbolSearchMatch[];
}

export interface SymbolCochange {
	id: number;
	name: string;
	kind: string;
	file_path: string;
	line_start: number;
	count: number;
}

export interface SymbolCochangesResponse {
	cochanges: SymbolCochange[];
	truncated: boolean;
}

export interface GraphDiffResponse {
	added: string[];
	removed: string[];
	unchanged_count: number;
	against: string;
	resolved_sha?: string;
	error?: string;
}

export interface HotspotItem {
	path: string;
	language: string;
	pagerank: number;
	churn: number;
	max_cc: number;
	avg_cc: number;
	score: number;
	health: number;
}

export interface HotspotsResponse {
	items: HotspotItem[];
	indexed: boolean;
}

export interface GodFunction {
	name: string;
	kind: string;
	path: string;
	language: string;
	line_start: number;
	line_end: number;
	lines: number;
	complexity: number;
}

export interface LongParams {
	name: string;
	kind: string;
	path: string;
	language: string;
	line_start: number;
	param_count: number;
	signature: string;
}

export interface SmellsResponse {
	god_functions: GodFunction[];
	long_params: LongParams[];
	indexed: boolean;
}

export interface CloneMember {
	id: number;
	name: string;
	kind: string;
	path: string;
	line_start: number;
	line_end: number;
}

export interface CloneGroup {
	shape_hash: string;
	member_count: number;
	avg_lines: number;
	members: CloneMember[];
}

export interface ClonesResponse {
	groups: CloneGroup[];
	indexed: boolean;
}

export interface DeadCodeItem {
	id: number;
	name: string;
	kind: string;
	path: string;
	language: string;
	line_start: number;
	is_exported: boolean;
}

export interface DeadCodeResponse {
	items: DeadCodeItem[];
	indexed: boolean;
	available: boolean;
}

export type HealthSeverity = 'critical' | 'medium' | 'low' | 'ok';

export interface HealthFile {
	path: string;
	language: string;
	health: number;
	max_cc: number;
	pagerank: number;
	churn: number;
	smell_count: number;
	severity: HealthSeverity;
}

export interface HealthSummary {
	avg_health: number;
	critical_count: number;
	medium_count: number;
	low_count: number;
	file_count: number;
}

export interface ProjectHealthResponse {
	files: HealthFile[];
	summary: HealthSummary;
	indexed: boolean;
}
