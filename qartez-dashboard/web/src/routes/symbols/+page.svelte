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
	import { page } from '$app/stores';
	import { dashboardSocket } from '$lib/ws.svelte';
	import { fetchSymbolGraph, triggerReindex } from '$lib/api';
	import type { SymbolGraphResponse } from '$lib/types';
	import SymbolPanel from '$lib/components/SymbolPanel.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import DatabaseZap from '@lucide/svelte/icons/database-zap';
	import { kindColor, fileColor, KIND_LEGEND } from '$lib/map/symbol-colors';
	import {
		createForceGraph,
		paintCanvas,
		hitTestQuadtree,
		type ForceGraphHandle,
		type ForceGraphNode,
		type ForceGraphLink
	} from '$lib/map/force-graph';

	type ColorBy = 'file' | 'kind';
	type RenderMode = 'auto' | 'svg' | 'canvas';

	interface SymNode extends ForceGraphNode {
		name: string;
		kind: string;
		file_id: number;
		file_path: string;
		pagerank: number;
		complexity: number | null;
	}

	type SymLink = ForceGraphLink<SymNode> & { kind: string };

	const CANVAS_AUTO_THRESHOLD = 200;

	interface PersistedView {
		k: number;
		x: number;
		y: number;
		selectedSymbolId: number | null;
	}

	const sock = dashboardSocket();
	const latest = $derived(sock.events[0]);

	let svgEl: SVGSVGElement | null = $state(null);
	let canvasEl: HTMLCanvasElement | null = $state(null);
	let containerEl: HTMLDivElement | null = $state(null);

	let graph = $state<SymbolGraphResponse | null>(null);
	let loadError = $state<string | null>(null);
	let hovered = $state<SymNode | null>(null);
	let tooltipX = $state(0);
	let tooltipY = $state(0);
	let colorBy = $state<ColorBy>('file');
	let selectedSymbolId = $state<number | null>(null);
	let renderMode = $state<RenderMode>('auto');
	let initialView: PersistedView | null = null;
	let selectedKinds = $state<Set<string>>(new Set());
	let neighborsOf = $state<number | null>(null);

	let handle: ForceGraphHandle<SymNode, SymLink> | null = null;
	let simNodes: SymNode[] = [];
	let simLinks: SymLink[] = [];
	let rafId: number | null = null;
	let zoomBehavior: ZoomBehavior<SVGSVGElement, unknown> | null = null;
	let canvasZoomBehavior: ZoomBehavior<HTMLCanvasElement, unknown> | null = null;
	let lastTransform = { k: 1, x: 0, y: 0 };
	let canvasTransform = $state({ k: 1, x: 0, y: 0 });
	let viewSize = { width: 800, height: 600 };

	const LS_COLOR = 'qartez:symbols:colorBy';
	const LS_VIEW = 'qartez:symbols:viewState';
	const LS_RENDERMODE = 'qartez:graph:renderMode';
	const LS_KIND = 'qartez:symbols:kindFilter';

	const effectiveRenderMode = $derived.by<'svg' | 'canvas'>(() => {
		if (renderMode === 'svg' || renderMode === 'canvas') return renderMode;
		const n = graph?.nodes.length ?? 0;
		return n > CANVAS_AUTO_THRESHOLD ? 'canvas' : 'svg';
	});

	function nodeColor(d: SymNode): string {
		return colorBy === 'kind' ? kindColor(d.kind) : fileColor(d.file_path);
	}

	function nodeRadius(d: SymNode): number {
		return 3.5 + Math.sqrt(Math.max(0, d.pagerank)) * 28;
	}

	function isSelected(d: SymNode): boolean {
		return selectedSymbolId !== null && d.id === selectedSymbolId;
	}

	const kinds = $derived.by(() => {
		if (!graph) return [] as string[];
		const counts = new Map<string, number>();
		for (const n of graph.nodes) {
			const k = (n.kind ?? '').toLowerCase();
			if (!k) continue;
			counts.set(k, (counts.get(k) ?? 0) + 1);
		}
		return [...counts.entries()]
			.sort((a, b) => (b[1] - a[1]) || a[0].localeCompare(b[0]))
			.map(([k]) => k);
	});

	function kindVisible(kind: string): boolean {
		return selectedKinds.size === 0 || selectedKinds.has(kind);
	}

	async function loadGraph(): Promise<void> {
		try {
			const data = await fetchSymbolGraph(200, colorBy, neighborsOf ?? undefined);
			graph = data;
			loadError = null;
		} catch (err) {
			loadError = (err as Error).message;
			graph = null;
		}
	}

	function buildSim(width: number, height: number): void {
		if (!graph) return;

		const filterSet = selectedKinds;
		const allowed = (k: string) => filterSet.size === 0 || filterSet.has(k.toLowerCase());
		const visibleIds = new Set<number>();
		simNodes = [];
		for (const n of graph.nodes) {
			if (!allowed(n.kind ?? '')) continue;
			visibleIds.add(n.id);
			simNodes.push({
				id: n.id,
				name: n.name,
				kind: n.kind,
				file_id: n.file_id,
				file_path: n.file_path,
				pagerank: n.pagerank,
				complexity: n.complexity
			});
		}
		simLinks = graph.links
			.filter((l) => visibleIds.has(l.source) && visibleIds.has(l.target))
			.map((l) => ({
				source: l.source,
				target: l.target,
				kind: l.kind
			}));

		handle = createForceGraph<SymNode, SymLink>({
			nodes: simNodes,
			links: simLinks,
			width,
			height,
			linkDistance: 50,
			linkStrength: 0.45,
			chargeStrength: -160,
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
			{ id: 'sym-arrow-call', color: 'var(--color-fg-muted)' },
			{ id: 'sym-arrow-type', color: 'var(--color-amber)' }
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
			.selectAll<SVGLineElement, SymLink>('line')
			.data(simLinks, (d) => {
				const s = typeof d.source === 'object' ? (d.source as SymNode).id : d.source;
				const t = typeof d.target === 'object' ? (d.target as SymNode).id : d.target;
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
				d.kind === 'type' ? 'var(--color-amber)' : 'var(--color-fg-muted)'
			)
			.attr('marker-end', (d) =>
				d.kind === 'type' ? 'url(#sym-arrow-type)' : 'url(#sym-arrow-call)'
			)
			.attr('stroke-opacity', 0.3)
			.attr('x1', (d) => (d.source as SymNode).x ?? 0)
			.attr('y1', (d) => (d.source as SymNode).y ?? 0)
			.attr('x2', (d) => {
				const s = d.source as SymNode;
				const t = d.target as SymNode;
				const trimmed = trimEndpoint(s.x ?? 0, s.y ?? 0, t.x ?? 0, t.y ?? 0, nodeRadius(t));
				return trimmed.x2;
			})
			.attr('y2', (d) => {
				const s = d.source as SymNode;
				const t = d.target as SymNode;
				const trimmed = trimEndpoint(s.x ?? 0, s.y ?? 0, t.x ?? 0, t.y ?? 0, nodeRadius(t));
				return trimmed.y2;
			});

		linkSel.exit().remove();

		const nodeSel = svg
			.select<SVGGElement>('g.nodes')
			.selectAll<SVGCircleElement, SymNode>('circle')
			.data(simNodes, (d) => d.id);

		const nodeEnter = nodeSel
			.enter()
			.append('circle')
			.attr('stroke-width', 1)
			.style('cursor', 'pointer')
			.on('mouseenter', (event: MouseEvent, d: SymNode) => {
				hovered = d;
				updateTooltip(event);
			})
			.on('mousemove', (event: MouseEvent) => {
				updateTooltip(event);
			})
			.on('mouseleave', () => {
				hovered = null;
			})
			.on('click', (_event: MouseEvent, d: SymNode) => {
				selectedSymbolId = d.id;
				persistView();
			});

		nodeEnter.call(
			drag<SVGCircleElement, SymNode>()
				.on('start', (event: D3DragEvent<SVGCircleElement, SymNode, SymNode>, d: SymNode) => {
					if (!event.active) handle?.simulation.alphaTarget(0.3).restart();
					d.fx = d.x;
					d.fy = d.y;
				})
				.on('drag', (event: D3DragEvent<SVGCircleElement, SymNode, SymNode>, d: SymNode) => {
					d.fx = event.x;
					d.fy = event.y;
				})
				.on('end', (event: D3DragEvent<SVGCircleElement, SymNode, SymNode>, d: SymNode) => {
					if (!event.active) handle?.simulation.alphaTarget(0);
					d.fx = null;
					d.fy = null;
				})
		);

		nodeEnter
			.merge(nodeSel)
			.attr('r', (d) => nodeRadius(d))
			.attr('fill', (d) => nodeColor(d))
			.attr('stroke', (d) => (isSelected(d) ? 'var(--color-amber)' : 'var(--color-bg)'))
			.attr('stroke-width', (d) => (isSelected(d) ? 2.5 : 1))
			.attr('cx', (d) => d.x ?? 0)
			.attr('cy', (d) => d.y ?? 0);

		nodeSel.exit().remove();
	}

	function renderCanvas(): void {
		if (!canvasEl) return;
		const ctx = canvasEl.getContext('2d');
		if (!ctx) return;
		paintCanvas<SymNode, SymLink>({
			ctx,
			width: viewSize.width,
			height: viewSize.height,
			transform: canvasTransform,
			nodes: simNodes,
			links: simLinks,
			nodeRadius,
			nodeFill: (d) => nodeColor(d),
			nodeStroke: (d) => (isSelected(d) ? 'var(--color-amber)' : null),
			nodeStrokeWidth: 2.5,
			linkStroke: (d) =>
				d.kind === 'type' ? 'var(--color-amber)' : 'var(--color-fg-muted)',
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
			const raw = localStorage.getItem(LS_COLOR);
			if (raw === 'file' || raw === 'kind') colorBy = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_VIEW);
			if (raw) {
				const parsed = JSON.parse(raw) as Partial<PersistedView>;
				if (
					typeof parsed.k === 'number' &&
					typeof parsed.x === 'number' &&
					typeof parsed.y === 'number'
				) {
					initialView = {
						k: parsed.k,
						x: parsed.x,
						y: parsed.y,
						selectedSymbolId:
							typeof parsed.selectedSymbolId === 'number' ? parsed.selectedSymbolId : null
					};
					if (initialView.selectedSymbolId !== null) {
						selectedSymbolId = initialView.selectedSymbolId;
					}
				}
			}
		} catch {
			/* ignore corrupt JSON */
		}
		try {
			const raw = localStorage.getItem(LS_RENDERMODE);
			if (raw === 'auto' || raw === 'svg' || raw === 'canvas') renderMode = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_KIND);
			if (raw) {
				const parsed = JSON.parse(raw);
				if (Array.isArray(parsed)) {
					selectedKinds = new Set(
						parsed.filter((x): x is string => typeof x === 'string').map((s) => s.toLowerCase())
					);
				}
			}
		} catch {
			/* ignore corrupt JSON */
		}
	}

	function persistColor(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_COLOR, colorBy);
		} catch {
			/* ignore */
		}
	}

	function persistView(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			const payload: PersistedView = {
				k: lastTransform.k,
				x: lastTransform.x,
				y: lastTransform.y,
				selectedSymbolId
			};
			localStorage.setItem(LS_VIEW, JSON.stringify(payload));
		} catch {
			/* ignore */
		}
	}

	function setColorBy(mode: ColorBy): void {
		if (colorBy === mode) return;
		colorBy = mode;
		persistColor();
		refreshGraph();
	}

	function persistKinds(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(LS_KIND, JSON.stringify([...selectedKinds]));
		} catch {
			/* ignore quota errors */
		}
	}

	function toggleKind(kind: string): void {
		const next = new Set(selectedKinds);
		if (next.has(kind)) next.delete(kind);
		else next.add(kind);
		selectedKinds = next;
		persistKinds();
		refreshGraph();
	}

	function resetKinds(): void {
		if (selectedKinds.size === 0) return;
		selectedKinds = new Set();
		persistKinds();
		refreshGraph();
	}

	function onPin(id: number): void {
		goto(`/?focusSymbol=${id}`);
	}

	async function onSymbolsReindex(): Promise<void> {
		try {
			await triggerReindex();
		} catch {
			/* error surfaces via the topbar reindex button */
		}
	}

	function onOpenFile(filePath: string): void {
		goto(`/map?focus=${encodeURIComponent(filePath)}`);
	}

	function onPivot(id: number): void {
		goto(`/symbols?neighbors_of=${id}`);
	}

	function clearPivot(): void {
		neighborsOf = null;
		goto('/symbols');
		refreshGraph();
	}

	function onClosePanel(): void {
		selectedSymbolId = null;
		persistView();
	}

	onMount(() => {
		loadPersisted();
		const param = $page.url.searchParams.get('neighbors_of');
		if (param !== null) {
			const parsed = parseInt(param, 10);
			if (Number.isFinite(parsed)) neighborsOf = parsed;
		}
		loadGraph();
	});

	function setupCanvas(width: number, height: number): void {
		if (!canvasEl) return;
		const dpr = window.devicePixelRatio || 1;
		canvasEl.width = Math.round(width * dpr);
		canvasEl.height = Math.round(height * dpr);
		canvasEl.style.width = `${width}px`;
		canvasEl.style.height = `${height}px`;
		const start =
			initialView !== null
				? zoomIdentity.translate(initialView.x, initialView.y).scale(initialView.k)
				: zoomIdentity;
		canvasZoomBehavior = zoom<HTMLCanvasElement, unknown>()
			.scaleExtent([0.2, 4])
			.on('zoom', (event: D3ZoomEvent<HTMLCanvasElement, unknown>) => {
				canvasTransform = { k: event.transform.k, x: event.transform.x, y: event.transform.y };
				lastTransform = canvasTransform;
				scheduleRender();
			})
			.on('end', () => persistView());
		select(canvasEl).call(canvasZoomBehavior).call(canvasZoomBehavior.transform, start);
		canvasTransform = { k: start.k, x: start.x, y: start.y };
		lastTransform = canvasTransform;
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
		const node = hitTestQuadtree<SymNode>(simNodes, canvasTransform, px, py, 12);
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
		const node = hitTestQuadtree<SymNode>(simNodes, canvasTransform, px, py, 12);
		if (node) {
			selectedSymbolId = node.id;
			persistView();
		}
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
			zoomBehavior = zoom<SVGSVGElement, unknown>()
				.scaleExtent([0.2, 4])
				.on('zoom', (event: D3ZoomEvent<SVGSVGElement, unknown>) => {
					inner.attr('transform', event.transform.toString());
					lastTransform = { k: event.transform.k, x: event.transform.x, y: event.transform.y };
				})
				.on('end', () => persistView());
			const startTransform =
				initialView !== null
					? zoomIdentity.translate(initialView.x, initialView.y).scale(initialView.k)
					: zoomIdentity;
			svg.call(zoomBehavior).call(zoomBehavior.transform, startTransform);
			lastTransform = { k: startTransform.k, x: startTransform.x, y: startTransform.y };
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
				zoomBehavior = null;
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
		colorBy;
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

		const filterSet = selectedKinds;
		const allowed = (k: string) => filterSet.size === 0 || filterSet.has(k.toLowerCase());
		const prev = new Map(simNodes.map((n) => [n.id, n]));
		const visibleIds = new Set<number>();
		simNodes = [];
		for (const n of graph.nodes) {
			if (!allowed(n.kind ?? '')) continue;
			visibleIds.add(n.id);
			const old = prev.get(n.id);
			simNodes.push({
				id: n.id,
				name: n.name,
				kind: n.kind,
				file_id: n.file_id,
				file_path: n.file_path,
				pagerank: n.pagerank,
				complexity: n.complexity,
				x: old?.x,
				y: old?.y,
				vx: old?.vx,
				vy: old?.vy
			});
		}
		simLinks = graph.links
			.filter((l) => visibleIds.has(l.source) && visibleIds.has(l.target))
			.map((l) => ({
				source: l.source,
				target: l.target,
				kind: l.kind
			}));

		const sim = handle.simulation;
		sim.nodes(simNodes);
		const linkForce = sim.force<ForceLink<SymNode, SymLink>>('link');
		linkForce?.links(simLinks);
		sim.force('center', forceCenter(width / 2, height / 2));
		sim.alpha(0.4).restart();
	}
</script>

<div bind:this={containerEl} class="symbols-container">
	<div class="controls">
		<div class="controls-row">
			<div class="seg" role="tablist" aria-label="color mode">
				<button
					type="button"
					role="tab"
					class="seg-btn"
					class:active={colorBy === 'file'}
					aria-selected={colorBy === 'file'}
					onclick={() => setColorBy('file')}
				>
					file
				</button>
				<button
					type="button"
					role="tab"
					class="seg-btn"
					class:active={colorBy === 'kind'}
					aria-selected={colorBy === 'kind'}
					onclick={() => setColorBy('kind')}
				>
					kind
				</button>
			</div>
		</div>
		{#if neighborsOf !== null}
			<div class="pivot-banner mono">
				<span>Pivoted around symbol #{neighborsOf}</span>
				<button type="button" class="pivot-clear" onclick={clearPivot}>show all</button>
			</div>
		{/if}
		{#if kinds.length > 0}
			<div class="chips">
				<button
					type="button"
					class="chip reset"
					class:active={selectedKinds.size === 0}
					onclick={resetKinds}
				>
					all
				</button>
				{#each kinds as kind (kind)}
					<button
						type="button"
						class="chip"
						class:off={!kindVisible(kind)}
						style="--chip-color: {kindColor(kind)}"
						onclick={() => toggleKind(kind)}
					>
						<span class="dot"></span>
						<span>{kind}</span>
					</button>
				{/each}
			</div>
		{/if}
	</div>

	<svg
		bind:this={svgEl}
		class="symbols-svg"
		class:gone={effectiveRenderMode !== 'svg'}
	>
		<g class="viewport">
			<g class="links"></g>
			<g class="nodes"></g>
		</g>
	</svg>
	<canvas
		bind:this={canvasEl}
		class="symbols-canvas"
		class:gone={effectiveRenderMode !== 'canvas'}
		onmousemove={onCanvasMove}
		onmouseleave={onCanvasLeave}
		onclick={onCanvasClick}
	></canvas>

	{#if graph?.truncated}
		<div class="badge truncated">
			showing top {graph.nodes.length} of {graph.nodes.length}+ symbols
		</div>
	{/if}

	{#if loadError}
		<div class="overlay">
			<div class="message">Failed to load symbol graph - {loadError}</div>
		</div>
	{:else if graph && graph.nodes.length === 0}
		<div class="overlay overlay-empty">
			<EmptyState
				icon={DatabaseZap}
				title="No symbol data yet"
				description="Run a fresh pass to populate the symbol graph."
				actionLabel="Reindex"
				onAction={onSymbolsReindex}
			/>
		</div>
	{:else if !graph}
		<div class="overlay">
			<div class="message muted">loading symbols...</div>
		</div>
	{/if}

	{#if hovered}
		<div class="tooltip" style="left: {tooltipX}px; top: {tooltipY}px;">
			<div class="tip-name">{hovered.name}</div>
			<div class="tip-meta">
				<span>{hovered.kind}</span>
				<span class="sep">·</span>
				<span class="tip-file">{hovered.file_path}</span>
				<span class="sep">·</span>
				<span>pr {hovered.pagerank.toFixed(3)}</span>
				{#if hovered.complexity !== null}
					<span class="sep">·</span>
					<span>complexity {hovered.complexity}</span>
				{/if}
			</div>
		</div>
	{/if}

	{#if graph && colorBy === 'kind'}
		<div class="legend legend-kind">
			<div class="legend-title">kind</div>
			<ul class="kind-list">
				{#each KIND_LEGEND as item (item.label)}
					<li>
						<span class="kind-swatch" style="background: {item.color};"></span>
						<span class="kind-label">{item.label}</span>
					</li>
				{/each}
			</ul>
		</div>
	{/if}

	<SymbolPanel
		symbolId={selectedSymbolId}
		onClose={onClosePanel}
		onPin={onPin}
		onOpenFile={onOpenFile}
		onPivot={onPivot}
	/>
</div>

<style>
	.symbols-container {
		position: relative;
		height: 100%;
		width: 100%;
		overflow: hidden;
		background: var(--color-bg);
	}

	.symbols-svg,
	.symbols-canvas {
		display: block;
		width: 100%;
		height: 100%;
	}

	.symbols-svg.gone,
	.symbols-canvas.gone {
		display: none;
	}

	.symbols-canvas {
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

	.controls-row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		pointer-events: auto;
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

	.pivot-banner {
		display: inline-flex;
		gap: 0.5rem;
		align-items: center;
		pointer-events: auto;
		font-size: 0.7rem;
		padding: 0.3rem 0.55rem;
		background: color-mix(in srgb, var(--color-amber) 10%, transparent);
		color: var(--color-amber);
		border: 1px solid var(--color-amber);
		border-radius: 0.25rem;
		align-self: flex-start;
	}

	.pivot-clear {
		font-family: inherit;
		font-size: inherit;
		background: transparent;
		color: var(--color-amber);
		border: 0;
		padding: 0;
		cursor: pointer;
		text-decoration: underline;
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
		max-width: 360px;
		z-index: 10;
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
	}

	.tip-name {
		color: var(--color-fg);
		word-break: break-all;
	}

	.tip-meta {
		margin-top: 0.2rem;
		color: var(--color-fg-muted);
		display: flex;
		flex-wrap: wrap;
		row-gap: 0.1rem;
	}

	.tip-file {
		word-break: break-all;
	}

	.sep {
		margin: 0 0.3rem;
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

	.kind-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		max-height: 18rem;
		overflow-y: auto;
	}

	.kind-list li {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.kind-swatch {
		width: 0.6rem;
		height: 0.6rem;
		border-radius: 50%;
		flex-shrink: 0;
	}

	.kind-label {
		color: var(--color-fg);
	}
</style>
