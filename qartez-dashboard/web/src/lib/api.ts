import type {
	FocusedFile,
	FocusedSymbol,
	ProjectSummary,
	GraphResponse,
	SymbolGraphResponse,
	SymbolSearchResponse,
	SymbolCochangesResponse,
	GraphDiffResponse,
	HotspotsResponse,
	SmellsResponse,
	ClonesResponse,
	DeadCodeResponse,
	ProjectHealthResponse
} from './types';

async function readError(r: Response, fallback: string): Promise<string> {
	let msg = r.statusText || `${fallback} ${r.status}`;
	try {
		const body = (await r.json()) as { error?: string };
		if (body.error) msg = body.error;
	} catch {
		/* leave default msg */
	}
	return msg;
}

export async function fetchProject(): Promise<ProjectSummary> {
	const r = await fetch('/api/project', { credentials: 'same-origin' });
	if (!r.ok) throw new Error(`/api/project ${r.status}`);
	return r.json();
}

export async function fetchHealth(): Promise<{ ok: boolean; version: string }> {
	const r = await fetch('/api/health', { credentials: 'same-origin' });
	if (!r.ok) throw new Error(`/api/health ${r.status}`);
	return r.json();
}

export async function fetchFocusedFile(path: string): Promise<FocusedFile> {
	const r = await fetch(`/api/focused-file?path=${encodeURIComponent(path)}`, {
		credentials: 'same-origin'
	});
	if (!r.ok) {
		let msg = r.statusText || `/api/focused-file ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export async function fetchGraph(
	limit?: number,
	withCochanges = false,
	neighborsOf?: string
): Promise<GraphResponse> {
	const params = new URLSearchParams();
	if (limit) params.set('limit', String(limit));
	if (withCochanges) params.set('with_cochanges', 'true');
	if (neighborsOf) params.set('neighbors_of', neighborsOf);
	const qs = params.toString() ? `?${params.toString()}` : '';
	const r = await fetch(`/api/graph${qs}`, { credentials: 'same-origin' });
	if (!r.ok) {
		let msg = r.statusText || `/api/graph ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export async function fetchSymbolGraph(
	limit?: number,
	colorBy?: 'file' | 'kind',
	neighborsOf?: number
): Promise<SymbolGraphResponse> {
	const params = new URLSearchParams();
	if (limit) params.set('limit', String(limit));
	if (colorBy) params.set('color_by', colorBy);
	if (neighborsOf !== undefined) params.set('neighbors_of', String(neighborsOf));
	const qs = params.toString() ? `?${params.toString()}` : '';
	const r = await fetch(`/api/symbol-graph${qs}`, { credentials: 'same-origin' });
	if (!r.ok) {
		let msg = r.statusText || `/api/symbol-graph ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export async function fetchFocusedSymbol(id: number): Promise<FocusedSymbol> {
	const r = await fetch(`/api/focused-symbol?id=${id}`, { credentials: 'same-origin' });
	if (!r.ok) {
		let msg = r.statusText || `/api/focused-symbol ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export async function fetchSymbolSearch(
	q: string,
	limit?: number,
	prefix?: boolean
): Promise<SymbolSearchResponse> {
	const params = new URLSearchParams();
	params.set('q', q);
	if (limit) params.set('limit', String(limit));
	if (prefix === true) params.set('prefix', 'true');
	const r = await fetch(`/api/symbol-search?${params.toString()}`, {
		credentials: 'same-origin'
	});
	if (!r.ok) {
		let msg = r.statusText || `/api/symbol-search ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export async function fetchSymbolCochanges(id: number): Promise<SymbolCochangesResponse> {
	const r = await fetch(`/api/symbol-cochanges?id=${id}`, { credentials: 'same-origin' });
	if (!r.ok) {
		let msg = r.statusText || `/api/symbol-cochanges ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export async function fetchGraphDiff(against: string): Promise<GraphDiffResponse> {
	const r = await fetch(`/api/graph-diff?against=${encodeURIComponent(against)}`, {
		credentials: 'same-origin'
	});
	if (!r.ok) {
		let msg = r.statusText || `/api/graph-diff ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}

export interface ReindexResponse {
	ok: boolean;
	in_progress: boolean;
	started_at: number;
}

export async function fetchHotspots(limit?: number): Promise<HotspotsResponse> {
	const qs = limit ? `?limit=${limit}` : '';
	const r = await fetch(`/api/hotspots${qs}`, { credentials: 'same-origin' });
	if (!r.ok) throw new Error(await readError(r, '/api/hotspots'));
	return r.json();
}

export async function fetchSmells(limit?: number): Promise<SmellsResponse> {
	const qs = limit ? `?limit=${limit}` : '';
	const r = await fetch(`/api/smells${qs}`, { credentials: 'same-origin' });
	if (!r.ok) throw new Error(await readError(r, '/api/smells'));
	return r.json();
}

export async function fetchClones(minLines?: number, limit?: number): Promise<ClonesResponse> {
	const params = new URLSearchParams();
	if (minLines !== undefined) params.set('min_lines', String(minLines));
	if (limit !== undefined) params.set('limit', String(limit));
	const qs = params.toString() ? `?${params.toString()}` : '';
	const r = await fetch(`/api/clones${qs}`, { credentials: 'same-origin' });
	if (!r.ok) throw new Error(await readError(r, '/api/clones'));
	return r.json();
}

export async function fetchDeadCode(limit?: number): Promise<DeadCodeResponse> {
	const qs = limit ? `?limit=${limit}` : '';
	const r = await fetch(`/api/dead-code${qs}`, { credentials: 'same-origin' });
	if (!r.ok) throw new Error(await readError(r, '/api/dead-code'));
	return r.json();
}

export async function fetchProjectHealth(limit?: number): Promise<ProjectHealthResponse> {
	const qs = limit ? `?limit=${limit}` : '';
	const r = await fetch(`/api/project-health${qs}`, { credentials: 'same-origin' });
	if (!r.ok) throw new Error(await readError(r, '/api/project-health'));
	return r.json();
}

export async function triggerReindex(): Promise<ReindexResponse> {
	const r = await fetch('/api/reindex', { method: 'POST', credentials: 'same-origin' });
	if (!r.ok) {
		let msg = r.statusText || `/api/reindex ${r.status}`;
		try {
			const body = (await r.json()) as { error?: string };
			if (body.error) msg = body.error;
		} catch {
			/* leave default msg */
		}
		throw new Error(msg);
	}
	return r.json();
}
