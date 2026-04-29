<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import {
		select,
		drag,
		zoom,
		zoomIdentity,
		forceCenter,
		type ForceLink,
		type D3DragEvent,
		type D3ZoomEvent,
		type ZoomBehavior
	} from 'd3';
	import { goto } from '$app/navigation';
	import { dashboardSocket } from '$lib/ws.svelte';
	import { fetchGraph, fetchSymbolSearch, fetchGraphDiff, triggerReindex } from '$lib/api';
	import type { GraphResponse, GraphDiffResponse } from '$lib/types';
	import FocusPanel from '$lib/components/FocusPanel.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import DatabaseZap from '@lucide/svelte/icons/database-zap';
	import { hotColor, clusterColor, HOT_LEGEND_STOPS } from '$lib/map/colors';
	import {
		createForceGraph,
		paintCanvas,
		hitTestQuadtree,
		type ForceGraphHandle,
		type ForceGraphNode,
		type ForceGraphLink
	} from '$lib/map/force-graph';

	type ViewMode = 'language' | 'hotspots' | 'clusters' | 'diff';
	type RenderMode = 'auto' | 'svg' | 'canvas';

	interface SimNode extends ForceGraphNode {
		path: string;
		language: string;
		pagerank: number;
		loc: number;
		hot_score: number;
		cluster_id: number | null;
	}

	type SimLink = ForceGraphLink<SimNode> & { kind: string };

	const CANVAS_AUTO_THRESHOLD = 200;

	const sock = dashboardSocket();
	const latest = $derived(sock.events[0]);

	let svgEl: SVGSVGElement | null = $state(null);
	let canvasEl: HTMLCanvasElement | null = $state(null);
	let containerEl: HTMLDivElement | null = $state(null);
	let tooltipEl: HTMLDivElement | null = $state(null);

	let graph = $state<GraphResponse | null>(null);
	let loadError = $state<string | null>(null);
	let hovered = $state<SimNode | null>(null);
	let tooltipX = $state(0);
	let tooltipY = $state(0);

	let deselectedLangs = $state<Set<string>>(new Set());
	let searchQuery = $state('');
	let withCochanges = $state(false);
	let selectedPath = $state<string | null>(null);
	let viewMode = $state<ViewMode>('language');
	let subgraphRoot = $state<string | null>(null);
	let symbolSearchFiles = $state<Set<string>>(new Set());
	let symbolSearchTimer: ReturnType<typeof setTimeout> | null = null;
	let symbolSearchSeq = 0;
	let renderMode = $state<RenderMode>('auto');
	let diffAgainst = $state('HEAD~10');
	let diffInput = $state('HEAD~10');
	let diffData = $state<GraphDiffResponse | null>(null);
	let diffError = $state<string | null>(null);
	let diffLoading = $state(false);

	let handle: ForceGraphHandle<SimNode, SimLink> | null = null;
	let simNodes: SimNode[] = [];
	let simLinks: SimLink[] = [];
	let rafId: number | null = null;
	let canvasZoomBehavior: ZoomBehavior<HTMLCanvasElement, unknown> | null = null;
	let svgZoomBehavior: ZoomBehavior<SVGSVGElement, unknown> | null = null;
	let canvasTransform = $state({ k: 1, x: 0, y: 0 });
	let viewSize = { width: 800, height: 600 };

	const LANG_COLORS: Record<string, string> = {
		rust: '#ce422b',
		typescript: '#3178c6',
		javascript: '#f7df1e',
		python: '#3776ab',
		go: '#00add8',
		java: '#f89820'
	};

	const LS_LANG = 'qartez:map:langFilter';
	const LS_SEARCH = 'qartez:map:search';
	const LS_COCHANGE = 'qartez:map:withCochanges';
	const LS_VIEWMODE = 'qartez:map:viewMode';
	const LS_SUBGRAPH = 'qartez:map:subgraph';
	const LS_RENDERMODE = 'qartez:graph:renderMode';
	const LS_DIFF_AGAINST = 'qartez:map:diffAgainst';

	const addedPaths = $derived.by(() => new Set(diffData?.added ?? []));
	const removedPaths = $derived.by(() => diffData?.removed ?? []);

	const effectiveRenderMode = $derived.by<'svg' | 'canvas'>(() => {
		if (renderMode === 'svg' || renderMode === 'canvas') return renderMode;
		const n = graph?.nodes.length ?? 0;
		return n > CANVAS_AUTO_THRESHOLD ? 'canvas' : 'svg';
	});

	function langOf(node: { language: string }): string {
		return (node.language ?? '').toLowerCase();
	}

	function langColor(d: SimNode): string {
		return LANG_COLORS[langOf(d)] ?? 'var(--color-fg-muted)';
	}

	function nodeColor(d: SimNode): string {
		if (viewMode === 'hotspots') return hotColor(d.hot_score);
		if (viewMode === 'clusters') {
			return d.cluster_id === null ? langColor(d) : clusterColor(d.cluster_id);
		}
		return langColor(d);
	}

	function nodeRadius(d: SimNode): number {
		return 4 + Math.sqrt(Math.max(0, d.pagerank)) * 30;
	}

	function nodeRingStroke(d: SimNode): string | null {
		if (viewMode === 'diff' && addedPaths.has(d.path)) return '#22c55e';
		if (searchActive() && searchMatches(d.path)) return 'var(--color-amber)';
		return null;
	}

	const langs = $derived.by(() => {
		if (!graph) return [] as string[];
		const set = new Set<string>();
		for (const n of graph.nodes) {
			const l = langOf(n);
			if (l) set.add(l);
		}
		return [...set].sort();
	});

	const clusterIds = $derived.by(() => {
		if (!graph) return [] as number[];
		const set = new Set<number>();
		for (const n of graph.nodes) {
			if (n.cluster_id !== null && n.cluster_id !== undefined) set.add(n.cluster_id);
		}
		return [...set].sort((a, b) => a - b);
	});

	function langVisible(lang: string): boolean {
		return !deselectedLangs.has(lang);
	}

	function searchActive(): boolean {
		return searchQuery.trim().length > 0;
	}

	function searchIsSymbolMode(): boolean {
		return searchQuery.trim().startsWith('@');
	}

	function searchMatches(path: string): boolean {
		if (!searchActive()) return true;
		if (searchIsSymbolMode()) return symbolSearchFiles.has(path);
		return path.toLowerCase().includes(searchQuery.trim().toLowerCase());
	}

	function nodeOpacity(d: SimNode): number {
		const langFactor = langVisible(langOf(d)) ? 1 : 0.1;
		const searchFactor = searchActive() ? (searchMatches(d.path) ? 1 : 0.4) : 1;
		return langFactor * searchFactor;
	}

	function linkOpacity(d: SimLink, base: number): number {
		const s = d.source as SimNode;
		const t = d.target as SimNode;
		const sLang = langVisible(langOf(s));
		const tLang = langVisible(langOf(t));
		const langFactor = sLang && tLang ? 1 : 0.1;
		let searchFactor = 1;
		if (searchActive()) {
			searchFactor = searchMatches(s.path) || searchMatches(t.path) ? 1 : 0.4;
		}
		return base * langFactor * searchFactor;
	}

	async function loadGraph(): Promise<void> {
		try {
			const data = await fetchGraph(200, withCochanges, subgraphRoot ?? undefined);
			graph = data;
			loadError = null;
		} catch (err) {
			loadError = (err as Error).message;
			graph = null;
		}
	}

	function buildLinks(): SimLink[] {
		if (!graph) return [];
		const links: SimLink[] = graph.links.map((l) => ({
			source: l.source,
			target: l.target,
			kind: l.kind
		}));
		if (withCochanges) {
			for (const c of graph.cochanges) {
				links.push({ source: c.source, target: c.target, kind: 'cochange' });
			}
		}
		return links;
	}

	function buildSim(width: number, height: number): void {
		if (!graph) return;

		simNodes = graph.nodes.map((n) => ({
			id: n.id,
			path: n.path,
			language: n.language,
			pagerank: n.pagerank,
			loc: n.loc,
			hot_score: n.hot_score,
			cluster_id: n.cluster_id
		}));
		simLinks = buildLinks();

		handle = createForceGraph<SimNode, SimLink>({
			nodes: simNodes,
			links: simLinks,
			width,
			height,
			linkDistance: 60,
			linkStrength: 0.4,
			chargeStrength: -180,
			collideRadius: (d) => nodeRadius(d) + 2,
			onTick: scheduleRender
		});
	}

	function ensureMarkers(): void {
		if (!svgEl) return;
		const svg = select(svgEl);
		let defs = svg.select<SVGDefsElement>('defs');
		if (defs.empty()) {
			defs = svg.append('defs');
		}
		const data = [
			{ id: 'arrow-import', color: 'var(--color-fg-muted)' },
			{ id: 'arrow-cochange', color: 'var(--color-amber)' }
		];
		const sel = defs.selectAll<SVGMarkerElement, (typeof data)[number]>('marker').data(data, (d) => d.id);
		const enter = sel
			.enter()
			.append('marker')
			.attr('id', (d) => d.id)
			.attr('viewBox', '0 -5 10 10')
			.attr('refX', 10)
			.attr('refY', 0)
			.attr('markerWidth', 6)
			.attr('markerHeight', 6)
			.attr('orient', 'auto');
		enter.append('path').attr('d', 'M0,-4 L8,0 L0,4 Z').attr('fill', (d) => d.color);
	}

	function scheduleRender(): void {
		if (rafId !== null) return;
		rafId = requestAnimationFrame(() => {
			rafId = null;
			render();
		});
	}

	function trimEndpoint(
		sx: number,
		sy: number,
		tx: number,
		ty: number,
		r: number
	): { x2: number; y2: number } {
		const dx = tx - sx;
		const dy = ty - sy;
		const len = Math.sqrt(dx * dx + dy * dy);
		if (len <= 0) return { x2: tx, y2: ty };
		const k = (r + 2) / len;
		return { x2: tx - dx * k, y2: ty - dy * k };
	}

	function renderSvg(): void {
		if (!svgEl) return;
		const svg = select(svgEl);
		const linkSel = svg
			.select<SVGGElement>('g.links')
			.selectAll<SVGLineElement, SimLink>('line')
			.data(simLinks, (d) => {
				const s = typeof d.source === 'object' ? (d.source as SimNode).id : d.source;
				const t = typeof d.target === 'object' ? (d.target as SimNode).id : d.target;
				return `${s}-${t}-${d.kind}`;
			});

		const linkEnter = linkSel
			.enter()
			.append('line')
			.attr('stroke-width', 1)
			.attr('fill', 'none');

		linkEnter
			.merge(linkSel)
			.attr('stroke', (d) =>
				d.kind === 'cochange' ? 'var(--color-amber)' : 'var(--color-fg-muted)'
			)
			.attr('stroke-dasharray', (d) => (d.kind === 'cochange' ? '4 3' : null))
			.attr('marker-end', (d) =>
				d.kind === 'cochange' ? 'url(#arrow-cochange)' : 'url(#arrow-import)'
			)
			.attr('stroke-opacity', (d) =>
				linkOpacity(d, d.kind === 'cochange' ? 0.5 : 0.25)
			)
			.attr('x1', (d) => (d.source as SimNode).x ?? 0)
			.attr('y1', (d) => (d.source as SimNode).y ?? 0)
			.attr('x2', (d) => {
				const s = d.source as SimNode;
				const t = d.target as SimNode;
				const trimmed = trimEndpoint(s.x ?? 0, s.y ?? 0, t.x ?? 0, t.y ?? 0, nodeRadius(t));
				return trimmed.x2;
			})
			.attr('y2', (d) => {
				const s = d.source as SimNode;
				const t = d.target as SimNode;
				const trimmed = trimEndpoint(s.x ?? 0, s.y ?? 0, t.x ?? 0, t.y ?? 0, nodeRadius(t));
				return trimmed.y2;
			});

		linkSel.exit().remove();

		const nodeSel = svg
			.select<SVGGElement>('g.nodes')
			.selectAll<SVGCircleElement, SimNode>('circle')
			.data(simNodes, (d) => d.id);

		const nodeEnter = nodeSel
			.enter()
			.append('circle')
			.attr('stroke-width', 1)
			.style('cursor', 'pointer')
			.on('mouseenter', (event: MouseEvent, d: SimNode) => {
				hovered = d;
				updateTooltip(event);
			})
			.on('mousemove', (event: MouseEvent) => {
				updateTooltip(event);
			})
			.on('mouseleave', () => {
				hovered = null;
			})
			.on('click', (_event: MouseEvent, d: SimNode) => {
				selectedPath = d.path;
			});

		nodeEnter.call(
			drag<SVGCircleElement, SimNode>()
				.on('start', (event: D3DragEvent<SVGCircleElement, SimNode, SimNode>, d: SimNode) => {
					if (!event.active) handle?.simulation.alphaTarget(0.3).restart();
					d.fx = d.x;
					d.fy = d.y;
				})
				.on('drag', (event: D3DragEvent<SVGCircleElement, SimNode, SimNode>, d: SimNode) => {
					d.fx = event.x;
					d.fy = event.y;
				})
				.on('end', (event: D3DragEvent<SVGCircleElement, SimNode, SimNode>, d: SimNode) => {
					if (!event.active) handle?.simulation.alphaTarget(0);
					d.fx = null;
					d.fy = null;
				})
		);

		nodeEnter
			.merge(nodeSel)
			.attr('r', (d) => nodeRadius(d))
			.attr('fill', (d) => nodeColor(d))
			.attr('stroke', (d) => nodeRingStroke(d) ?? 'var(--color-bg)')
			.attr('stroke-width', (d) => (nodeRingStroke(d) ? 2 : 1))
			.attr('opacity', (d) => nodeOpacity(d))
			.attr('cx', (d) => d.x ?? 0)
			.attr('cy', (d) => d.y ?? 0);

		nodeSel.exit().remove();
	}

	function renderCanvas(): void {
		if (!canvasEl) return;
		const ctx = canvasEl.getContext('2d');
		if (!ctx) return;
		paintCanvas<SimNode, SimLink>({
			ctx,
			width: viewSize.width,
			height: viewSize.height,
			transform: canvasTransform,
			nodes: simNodes,
			links: simLinks,
			nodeRadius,
			nodeFill: (d) => nodeColor(d),
			nodeStroke: (d) => nodeRingStroke(d),
			nodeStrokeWidth: 2,
			linkStroke: (d) =>
				d.kind === 'cochange' ? 'var(--color-amber)' : 'var(--color-fg-muted)',
			linkOpacity: 0.3,
			highlightedNodeId: hovered?.id ?? null
		});
	}

	function render(): void {
		if (effectiveRenderMode === 'canvas') renderCanvas();
		else renderSvg();
	}

	function updateTooltip(event: MouseEvent): void {
		if (!containerEl) return;
		const rect = containerEl.getBoundingClientRect();
		tooltipX = event.clientX - rect.left + 12;
		tooltipY = event.clientY - rect.top + 12;
	}

	function loadPersisted(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			const raw = localStorage.getItem(LS_LANG);
			if (raw) {
				const parsed = JSON.parse(raw);
				if (Array.isArray(parsed)) {
					deselectedLangs = new Set(parsed.filter((x): x is string => typeof x === 'string'));
				}
			}
		} catch {
			/* ignore corrupt JSON */
		}
		try {
			const raw = localStorage.getItem(LS_SEARCH);
			if (raw !== null) searchQuery = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_COCHANGE);
			if (raw !== null) withCochanges = raw === 'true';
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_VIEWMODE);
			if (raw === 'language' || raw === 'hotspots' || raw === 'clusters' || raw === 'diff') {
				viewMode = raw;
			}
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_SUBGRAPH);
			if (raw !== null && raw.length > 0) subgraphRoot = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_RENDERMODE);
			if (raw === 'auto' || raw === 'svg' || raw === 'canvas') renderMode = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_DIFF_AGAINST);
			if (raw !== null && raw.length > 0) {
				diffAgainst = raw;
				diffInput = raw;
			}
		} catch {
			/* ignore */
		}
	}

	function persistLangs(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_LANG, JSON.stringify([...deselectedLangs]));
		} catch {
			/* ignore quota errors */
		}
	}

	function persistSearch(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_SEARCH, searchQuery);
		} catch {
			/* ignore */
		}
	}

	function persistCochange(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_COCHANGE, withCochanges ? 'true' : 'false');
		} catch {
			/* ignore */
		}
	}

	function persistViewMode(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_VIEWMODE, viewMode);
		} catch {
			/* ignore */
		}
	}

	function persistSubgraph(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			if (subgraphRoot === null) localStorage.removeItem(LS_SUBGRAPH);
			else localStorage.setItem(LS_SUBGRAPH, subgraphRoot);
		} catch {
			/* ignore */
		}
	}

	function persistDiffAgainst(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_DIFF_AGAINST, diffAgainst);
		} catch {
			/* ignore */
		}
	}

	function setViewMode(mode: ViewMode): void {
		if (viewMode === mode) return;
		viewMode = mode;
		persistViewMode();
		if (mode === 'diff' && !diffData && !diffLoading) {
			loadDiff();
		}
	}

	async function loadDiff(): Promise<void> {
		diffLoading = true;
		diffError = null;
		try {
			const data = await fetchGraphDiff(diffAgainst);
			diffData = data;
			if (data.error) diffError = data.error;
		} catch (err) {
			diffError = (err as Error).message;
			diffData = null;
		} finally {
			diffLoading = false;
			scheduleRender();
		}
	}

	function applyDiffInput(): void {
		const trimmed = diffInput.trim();
		if (!trimmed || trimmed === diffAgainst) return;
		diffAgainst = trimmed;
		persistDiffAgainst();
		loadDiff();
	}

	function onExplore(path: string): void {
		subgraphRoot = path;
		persistSubgraph();
		selectedPath = null;
		refreshGraph();
	}

	function clearSubgraph(): void {
		subgraphRoot = null;
		persistSubgraph();
		refreshGraph();
	}

	function toggleLang(lang: string): void {
		const next = new Set(deselectedLangs);
		if (next.has(lang)) next.delete(lang);
		else next.add(lang);
		deselectedLangs = next;
		persistLangs();
		scheduleRender();
	}

	function resetLangs(): void {
		deselectedLangs = new Set();
		persistLangs();
		scheduleRender();
	}

	function onSearchInput(e: Event): void {
		searchQuery = (e.currentTarget as HTMLInputElement).value;
		persistSearch();
		scheduleSymbolSearch();
		scheduleRender();
	}

	function scheduleSymbolSearch(): void {
		if (symbolSearchTimer !== null) {
			clearTimeout(symbolSearchTimer);
			symbolSearchTimer = null;
		}
		const trimmed = searchQuery.trim();
		if (!trimmed.startsWith('@')) {
			if (symbolSearchFiles.size > 0) symbolSearchFiles = new Set();
			return;
		}
		const term = trimmed.slice(1).trim();
		if (term.length === 0) {
			if (symbolSearchFiles.size > 0) symbolSearchFiles = new Set();
			return;
		}
		const seq = ++symbolSearchSeq;
		symbolSearchTimer = setTimeout(async () => {
			symbolSearchTimer = null;
			try {
				const res = await fetchSymbolSearch(term, 50, true);
				if (seq !== symbolSearchSeq) return;
				symbolSearchFiles = new Set(res.matches.map((m) => m.file_path));
				scheduleRender();
			} catch {
				if (seq !== symbolSearchSeq) return;
				symbolSearchFiles = new Set();
				scheduleRender();
			}
		}, 150);
	}

	async function toggleCochanges(): Promise<void> {
		withCochanges = !withCochanges;
		persistCochange();
		await refreshGraph();
	}

	function onPin(path: string): void {
		goto(`/?focus=${encodeURIComponent(path)}`);
	}

	async function onMapReindex(): Promise<void> {
		try {
			await triggerReindex();
		} catch {
			/* error surfaces via the topbar reindex button */
		}
	}

	onMount(() => {
		loadPersisted();
		loadGraph();
		if (searchQuery.trim().startsWith('@')) scheduleSymbolSearch();
		if (viewMode === 'diff') loadDiff();
	});

	function setupCanvas(width: number, height: number): void {
		if (!canvasEl) return;
		const dpr = window.devicePixelRatio || 1;
		canvasEl.width = Math.round(width * dpr);
		canvasEl.height = Math.round(height * dpr);
		canvasEl.style.width = `${width}px`;
		canvasEl.style.height = `${height}px`;
		canvasZoomBehavior = zoom<HTMLCanvasElement, unknown>()
			.scaleExtent([0.2, 4])
			.on('zoom', (event: D3ZoomEvent<HTMLCanvasElement, unknown>) => {
				canvasTransform = { k: event.transform.k, x: event.transform.x, y: event.transform.y };
				scheduleRender();
			});
		select(canvasEl).call(canvasZoomBehavior).call(canvasZoomBehavior.transform, zoomIdentity);
	}

	function teardownCanvas(): void {
		if (canvasEl) select(canvasEl).on('.zoom', null);
		canvasZoomBehavior = null;
	}

	function onCanvasMove(event: MouseEvent): void {
		if (!canvasEl) return;
		const rect = canvasEl.getBoundingClientRect();
		const px = event.clientX - rect.left;
		const py = event.clientY - rect.top;
		const node = hitTestQuadtree<SimNode>(simNodes, canvasTransform, px, py, 12);
		if (node !== hovered) {
			hovered = node;
			scheduleRender();
		}
		if (node) updateTooltip(event);
	}

	function onCanvasLeave(): void {
		if (hovered !== null) {
			hovered = null;
			scheduleRender();
		}
	}

	function onCanvasClick(event: MouseEvent): void {
		if (!canvasEl) return;
		const rect = canvasEl.getBoundingClientRect();
		const px = event.clientX - rect.left;
		const py = event.clientY - rect.top;
		const node = hitTestQuadtree<SimNode>(simNodes, canvasTransform, px, py, 12);
		if (node) selectedPath = node.path;
	}

	$effect(() => {
		if (!containerEl || !graph) return;
		const rect = containerEl.getBoundingClientRect();
		const width = rect.width || 800;
		const height = rect.height || 600;
		viewSize = { width, height };

		const mode = effectiveRenderMode;
		if (mode === 'svg') {
			if (!svgEl) return;
			const svg = select(svgEl);
			svg.attr('viewBox', `0 0 ${width} ${height}`).attr('width', width).attr('height', height);
			ensureMarkers();
			const inner = svg.select<SVGGElement>('g.viewport');
			svgZoomBehavior = zoom<SVGSVGElement, unknown>()
				.scaleExtent([0.2, 4])
				.on('zoom', (event: D3ZoomEvent<SVGSVGElement, unknown>) => {
					inner.attr('transform', event.transform.toString());
				});
			svg.call(svgZoomBehavior).call(svgZoomBehavior.transform, zoomIdentity);
		} else {
			setupCanvas(width, height);
		}

		buildSim(width, height);

		return () => {
			if (rafId !== null) {
				cancelAnimationFrame(rafId);
				rafId = null;
			}
			handle?.destroy();
			handle = null;
			if (mode === 'svg' && svgEl) {
				const svg = select(svgEl);
				svg.on('.zoom', null);
				svg.select('g.links').selectAll('*').remove();
				svg.select('g.nodes').selectAll('*').remove();
				svg.select('defs').selectAll('*').remove();
				svgZoomBehavior = null;
			} else {
				teardownCanvas();
			}
		};
	});

	$effect(() => {
		const evt = latest;
		if (!evt) return;
		untrack(() => {
			if (evt.type === 'index_updated') {
				refreshGraph();
			}
		});
	});

	$effect(() => {
		// Re-render fills when the view mode changes.
		viewMode;
		untrack(() => {
			if (svgEl && graph) scheduleRender();
		});
	});

	async function refreshGraph(): Promise<void> {
		if (!containerEl) {
			await loadGraph();
			return;
		}
		const rect = containerEl.getBoundingClientRect();
		const width = rect.width || 800;
		const height = rect.height || 600;

		await loadGraph();
		if (!graph || !handle) return;

		const prev = new Map(simNodes.map((n) => [n.id, n]));
		simNodes = graph.nodes.map((n) => {
			const old = prev.get(n.id);
			return {
				id: n.id,
				path: n.path,
				language: n.language,
				pagerank: n.pagerank,
				loc: n.loc,
				hot_score: n.hot_score,
				cluster_id: n.cluster_id,
				x: old?.x,
				y: old?.y,
				vx: old?.vx,
				vy: old?.vy
			};
		});
		simLinks = buildLinks();

		const sim = handle.simulation;
		sim.nodes(simNodes);
		const linkForce = sim.force<ForceLink<SimNode, SimLink>>('link');
		linkForce?.links(simLinks);
		sim.force('center', forceCenter(width / 2, height / 2));
		sim.alpha(0.5).restart();
	}
</script>

<div bind:this={containerEl} class="map-container">
	<div class="controls">
		{#if subgraphRoot}
			<div class="subgraph-row">
				<button type="button" class="chip back" onclick={clearSubgraph}>
					<span aria-hidden="true">←</span>
					<span>back to full map</span>
				</button>
				<span class="subgraph-label" title={subgraphRoot}>{subgraphRoot}</span>
			</div>
		{/if}
		<div class="search-row">
			<div class="seg" role="tablist" aria-label="view mode">
				<button
					type="button"
					role="tab"
					class="seg-btn"
					class:active={viewMode === 'language'}
					aria-selected={viewMode === 'language'}
					onclick={() => setViewMode('language')}
				>
					language
				</button>
				<button
					type="button"
					role="tab"
					class="seg-btn"
					class:active={viewMode === 'hotspots'}
					aria-selected={viewMode === 'hotspots'}
					onclick={() => setViewMode('hotspots')}
				>
					hotspots
				</button>
				<button
					type="button"
					role="tab"
					class="seg-btn"
					class:active={viewMode === 'clusters'}
					aria-selected={viewMode === 'clusters'}
					onclick={() => setViewMode('clusters')}
				>
					clusters
				</button>
				<button
					type="button"
					role="tab"
					class="seg-btn"
					class:active={viewMode === 'diff'}
					aria-selected={viewMode === 'diff'}
					onclick={() => setViewMode('diff')}
				>
					diff
				</button>
			</div>
			<input
				class="search"
				type="text"
				placeholder="search path..."
				value={searchQuery}
				oninput={onSearchInput}
			/>
			{#if viewMode === 'diff'}
				<div class="diff-row">
					<input
						class="diff-input"
						type="text"
						placeholder="HEAD~10"
						bind:value={diffInput}
						onkeydown={(e) => {
							if (e.key === 'Enter') applyDiffInput();
						}}
					/>
					<button type="button" class="diff-apply" onclick={applyDiffInput}>apply</button>
				</div>
			{/if}
			<label class="cochange-toggle">
				<input
					type="checkbox"
					checked={withCochanges}
					onchange={toggleCochanges}
				/>
				<span>show co-changes</span>
			</label>
		</div>
		{#if langs.length > 0}
			<div class="chips">
				<button
					type="button"
					class="chip reset"
					class:active={deselectedLangs.size === 0}
					onclick={resetLangs}
				>
					all
				</button>
				{#each langs as lang (lang)}
					<button
						type="button"
						class="chip"
						class:off={!langVisible(lang)}
						style="--chip-color: {LANG_COLORS[lang] ?? 'var(--color-fg-muted)'}"
						onclick={() => toggleLang(lang)}
					>
						<span class="dot"></span>
						<span>{lang}</span>
					</button>
				{/each}
			</div>
		{/if}
	</div>

	<svg bind:this={svgEl} class="map-svg" class:gone={effectiveRenderMode !== 'svg'}>
		<g class="viewport">
			<g class="links"></g>
			<g class="nodes"></g>
		</g>
	</svg>
	<canvas
		bind:this={canvasEl}
		class="map-canvas"
		class:gone={effectiveRenderMode !== 'canvas'}
		onmousemove={onCanvasMove}
		onmouseleave={onCanvasLeave}
		onclick={onCanvasClick}
	></canvas>

	{#if graph?.truncated}
		<div class="badge truncated">showing top {graph.nodes.length} files</div>
	{/if}

	{#if loadError}
		<div class="overlay">
			<div class="message">Failed to load graph - {loadError}</div>
		</div>
	{:else if graph && graph.nodes.length === 0}
		<div class="overlay overlay-empty">
			<EmptyState
				icon={DatabaseZap}
				title="No graph data yet"
				description="Run a fresh pass to populate the file graph."
				actionLabel="Reindex"
				onAction={onMapReindex}
			/>
		</div>
	{:else if !graph}
		<div class="overlay">
			<div class="message muted">loading graph...</div>
		</div>
	{/if}

	{#if hovered}
		<div
			bind:this={tooltipEl}
			class="tooltip"
			style="left: {tooltipX}px; top: {tooltipY}px;"
		>
			<div class="tip-path">{hovered.path}</div>
			<div class="tip-meta">
				<span>{hovered.language}</span>
				<span class="sep">·</span>
				<span>{hovered.loc} lines</span>
				<span class="sep">·</span>
				<span>pr {hovered.pagerank.toFixed(3)}</span>
				{#if viewMode === 'hotspots'}
					<span class="sep">·</span>
					<span>hot {hovered.hot_score.toFixed(2)}</span>
				{/if}
				{#if viewMode === 'clusters' && hovered.cluster_id !== null}
					<span class="sep">·</span>
					<span>cluster {hovered.cluster_id}</span>
				{/if}
			</div>
		</div>
	{/if}

	{#if graph && viewMode === 'hotspots'}
		<div class="legend legend-hot">
			<div class="legend-title">hot score</div>
			<div class="legend-ramp">
				{#each HOT_LEGEND_STOPS as stop, i (i)}
					<span class="ramp-swatch" style="background: {stop.color};"></span>
				{/each}
			</div>
			<div class="legend-bounds">
				<span>low</span>
				<span>high</span>
			</div>
		</div>
	{/if}

	{#if graph && viewMode === 'clusters' && clusterIds.length > 0}
		<div class="legend legend-clusters">
			<div class="legend-title">clusters</div>
			<ul class="cluster-list">
				{#each clusterIds as cid (cid)}
					<li>
						<span class="cluster-swatch" style="background: {clusterColor(cid)};"></span>
						<span class="cluster-label">cluster {cid}</span>
					</li>
				{/each}
			</ul>
		</div>
	{/if}

	{#if viewMode === 'diff'}
		<div class="legend legend-diff">
			<div class="legend-title">diff vs {diffAgainst}</div>
			{#if diffLoading}
				<div class="diff-status muted">loading...</div>
			{:else if diffError}
				<div class="diff-status err">diff failed: {diffError}</div>
			{:else if diffData}
				<div class="diff-counts">
					<span class="d-add">+ {diffData.added.length} added</span>
					<span class="d-rm">- {diffData.removed.length} removed</span>
					<span class="d-eq">= {diffData.unchanged_count} unchanged</span>
				</div>
				{#if removedPaths.length > 0}
					<div class="diff-removed-title">removed</div>
					<ul class="diff-removed">
						{#each removedPaths.slice(0, 12) as path (path)}
							<li title={path}>{path}</li>
						{/each}
						{#if removedPaths.length > 12}
							<li class="muted">... +{removedPaths.length - 12} more</li>
						{/if}
					</ul>
				{/if}
			{/if}
		</div>
	{/if}

	<FocusPanel
		path={selectedPath}
		onClose={() => (selectedPath = null)}
		onPin={onPin}
		onExplore={onExplore}
	/>
</div>

<style>
	.map-container {
		position: relative;
		height: 100%;
		width: 100%;
		overflow: hidden;
		background: var(--color-bg);
	}

	.map-svg,
	.map-canvas {
		display: block;
		width: 100%;
		height: 100%;
	}

	.map-canvas.gone,
	.map-svg.gone {
		display: none;
	}

	.map-canvas {
		cursor: pointer;
	}

	.controls {
		position: absolute;
		top: 0.5rem;
		left: 0.5rem;
		right: 0.5rem;
		z-index: 5;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		pointer-events: none;
	}

	.search-row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		pointer-events: auto;
	}

	.search {
		flex: 0 1 280px;
		font-family: var(--font-mono, monospace);
		font-size: 0.75rem;
		padding: 0.35rem 0.55rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		outline: none;
	}

	.search:focus {
		border-color: var(--color-amber);
	}

	.cochange-toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		color: var(--color-fg-muted);
		padding: 0.3rem 0.5rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		cursor: pointer;
		user-select: none;
	}

	.cochange-toggle input {
		margin: 0;
		cursor: pointer;
	}

	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		pointer-events: auto;
	}

	.chip {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		padding: 0.25rem 0.5rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 999px;
		cursor: pointer;
		transition: opacity 0.15s ease;
	}

	.chip:hover {
		border-color: var(--color-amber);
	}

	.chip.off {
		opacity: 0.4;
	}

	.chip.reset {
		color: var(--color-fg-muted);
	}

	.chip.reset.active {
		color: var(--color-amber);
		border-color: var(--color-amber);
	}

	.chip .dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--chip-color, var(--color-fg-muted));
	}

	.badge {
		position: absolute;
		top: 0.75rem;
		right: 0.75rem;
		padding: 0.25rem 0.5rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		color: var(--color-fg-muted);
		background: color-mix(in srgb, var(--color-bg) 80%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
	}

	.overlay {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		pointer-events: none;
	}

	.overlay-empty {
		pointer-events: auto;
	}

	.message {
		font-family: var(--font-mono, monospace);
		font-size: 0.875rem;
		color: var(--color-amber);
		padding: 0.75rem 1rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.375rem;
	}

	.message.muted {
		color: var(--color-fg-muted);
	}

	.tooltip {
		position: absolute;
		pointer-events: none;
		font-family: var(--font-mono, monospace);
		font-size: 0.75rem;
		padding: 0.4rem 0.55rem;
		background: var(--color-surface);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		max-width: 320px;
		z-index: 10;
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
	}

	.tip-path {
		color: var(--color-fg);
		word-break: break-all;
	}

	.tip-meta {
		margin-top: 0.2rem;
		color: var(--color-fg-muted);
	}

	.sep {
		margin: 0 0.3rem;
	}

	.subgraph-row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		pointer-events: auto;
	}

	.subgraph-label {
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		color: var(--color-fg-muted);
		max-width: 28rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.chip.back {
		color: var(--color-amber);
		border-color: var(--color-amber);
	}

	.seg {
		display: inline-flex;
		pointer-events: auto;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		overflow: hidden;
	}

	.seg-btn {
		padding: 0.3rem 0.6rem;
		background: transparent;
		color: var(--color-fg-muted);
		border: 0;
		border-right: 1px solid var(--color-border);
		cursor: pointer;
		font-family: inherit;
		font-size: inherit;
	}

	.seg-btn:last-child {
		border-right: 0;
	}

	.seg-btn:hover {
		color: var(--color-fg);
	}

	.seg-btn.active {
		color: var(--color-amber);
		background: color-mix(in srgb, var(--color-amber) 10%, transparent);
	}

	.legend {
		position: absolute;
		bottom: 0.75rem;
		right: 0.75rem;
		z-index: 5;
		padding: 0.45rem 0.55rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		color: var(--color-fg-muted);
		background: color-mix(in srgb, var(--color-bg) 88%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		max-width: 14rem;
	}

	.legend-title {
		color: var(--color-fg);
		margin-bottom: 0.3rem;
		font-size: 0.65rem;
		text-transform: lowercase;
		letter-spacing: 0.04em;
	}

	.legend-ramp {
		display: flex;
		gap: 2px;
	}

	.ramp-swatch {
		flex: 1;
		height: 0.6rem;
		border-radius: 1px;
	}

	.legend-bounds {
		display: flex;
		justify-content: space-between;
		margin-top: 0.2rem;
	}

	.cluster-list {
		list-style: none;
		margin: 0;
		padding: 0;
		max-height: 14rem;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.cluster-list li {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.cluster-swatch {
		width: 0.6rem;
		height: 0.6rem;
		border-radius: 2px;
		flex-shrink: 0;
	}

	.cluster-label {
		color: var(--color-fg);
	}

	.diff-row {
		display: inline-flex;
		gap: 0.3rem;
		pointer-events: auto;
	}

	.diff-input {
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		padding: 0.3rem 0.5rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		outline: none;
		width: 9rem;
	}

	.diff-input:focus {
		border-color: var(--color-amber);
	}

	.diff-apply {
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		padding: 0.3rem 0.6rem;
		background: color-mix(in srgb, var(--color-amber) 12%, transparent);
		color: var(--color-amber);
		border: 1px solid var(--color-amber);
		border-radius: 0.25rem;
		cursor: pointer;
	}

	.diff-counts {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.d-add {
		color: #22c55e;
	}

	.d-rm {
		color: #ef4444;
	}

	.d-eq {
		color: var(--color-fg-muted);
	}

	.diff-status.err {
		color: #ef4444;
	}

	.diff-status.muted {
		color: var(--color-fg-muted);
	}

	.diff-removed-title {
		margin-top: 0.4rem;
		color: var(--color-fg);
		font-size: 0.65rem;
	}

	.diff-removed {
		list-style: none;
		margin: 0.2rem 0 0 0;
		padding: 0;
		max-height: 8rem;
		overflow-y: auto;
		color: #ef4444;
	}

	.diff-removed li {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 13rem;
	}

	.diff-removed li.muted {
		color: var(--color-fg-muted);
	}
</style>
