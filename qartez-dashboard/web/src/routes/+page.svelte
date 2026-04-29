<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { select, range } from 'd3';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import { dashboardSocket } from '$lib/ws.svelte';
	import { fetchProject, fetchFocusedFile, triggerReindex } from '$lib/api';
	import type { ProjectSummary, DashboardEvent, FocusedFile } from '$lib/types';
	import SymbolPanel from '$lib/components/SymbolPanel.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import DatabaseZap from '@lucide/svelte/icons/database-zap';

	async function onPulseReindex(): Promise<void> {
		try {
			await triggerReindex();
		} catch {
			/* error surfaces via the topbar reindex button */
		}
	}

	const sock = dashboardSocket();

	let project = $state<ProjectSummary | null>(null);
	let projectError = $state<string | null>(null);

	let focusedPath = $state('');
	let focused = $state<FocusedFile | null>(null);
	let focusedError = $state<string | null>(null);
	let reindexActive = $state(false);
	let pinnedSymbolId = $state<number | null>(null);

	function parsePinnedSymbol(raw: string | null): number | null {
		if (raw === null) return null;
		const n = parseInt(raw, 10);
		if (!Number.isFinite(n) || n <= 0) return null;
		return n;
	}

	$effect(() => {
		const raw = page.url.searchParams.get('focusSymbol');
		const next = parsePinnedSymbol(raw);
		untrack(() => {
			if (pinnedSymbolId !== next) pinnedSymbolId = next;
		});
	});

	function onPinSymbol(id: number): void {
		pinnedSymbolId = id;
	}

	function onClosePinned(): void {
		pinnedSymbolId = null;
		goto('/');
	}

	function onOpenSymbolFile(filePath: string): void {
		goto(`/?focus=${encodeURIComponent(filePath)}`);
	}

	function onPivotSymbol(id: number): void {
		goto(`/symbols?neighbors_of=${id}`);
	}

	const recent = $derived(sock.events.slice(0, 20));
	const latest = $derived(sock.events[0]);

	async function loadProject(): Promise<void> {
		try {
			project = await fetchProject();
			projectError = null;
		} catch (err) {
			projectError = (err as Error).message;
		}
	}

	async function loadFocused(): Promise<void> {
		const p = focusedPath.trim();
		if (!p) {
			focused = null;
			focusedError = null;
			return;
		}
		try {
			focused = await fetchFocusedFile(p);
			focusedError = null;
		} catch (err) {
			focused = null;
			focusedError = (err as Error).message;
		}
	}

	function onPathKey(e: KeyboardEvent): void {
		if (e.key === 'Enter') {
			e.preventDefault();
			loadFocused();
		}
	}

	onMount(() => {
		loadProject();
		const focus = page.url.searchParams.get('focus');
		if (focus) {
			focusedPath = focus;
			loadFocused();
		}
	});

	$effect(() => {
		const evt = latest;
		if (!evt) return;
		untrack(() => {
			if (evt.type === 'index_updated') {
				loadProject();
				if (focusedPath.trim()) loadFocused();
			} else if (evt.type === 'reindex_progress') {
				if (evt.data.phase === 'complete') {
					reindexActive = false;
				} else {
					reindexActive = true;
				}
			}
		});
	});

	let ringEl: SVGSVGElement | null = $state(null);
	const direct = $derived(focused?.impact.direct ?? null);

	$effect(() => {
		if (!ringEl) return;

		const svg = select(ringEl);
		const size = 220;
		const cx = size / 2;
		const cy = size / 2;
		const tickCount = 24;
		const innerR = 78;
		const outerR = 96;

		svg.attr('viewBox', `0 0 ${size} ${size}`).attr('width', size).attr('height', size);

		svg
			.selectAll('line')
			.data(range(tickCount))
			.enter()
			.append('line')
			.attr('x1', (i) => {
				const angle = (i / tickCount) * Math.PI * 2 - Math.PI / 2;
				return cx + Math.cos(angle) * innerR;
			})
			.attr('y1', (i) => {
				const angle = (i / tickCount) * Math.PI * 2 - Math.PI / 2;
				return cy + Math.sin(angle) * innerR;
			})
			.attr('x2', (i) => {
				const angle = (i / tickCount) * Math.PI * 2 - Math.PI / 2;
				return cx + Math.cos(angle) * outerR;
			})
			.attr('y2', (i) => {
				const angle = (i / tickCount) * Math.PI * 2 - Math.PI / 2;
				return cy + Math.sin(angle) * outerR;
			})
			.attr('stroke', 'var(--color-amber)')
			.attr('stroke-width', 2)
			.attr('stroke-linecap', 'round')
			.attr('opacity', 0.25);

		if (direct !== null) {
			const lit = Math.min(direct, tickCount);
			svg
				.selectAll<SVGLineElement, number>('line')
				.attr('opacity', (_, i) => (i < lit ? 1 : 0.25));
			return () => {
				svg.selectAll('*').remove();
			};
		}

		const interval = setInterval(() => {
			const idx = Math.floor(Math.random() * tickCount);
			const ticks = svg.selectAll<SVGLineElement, number>('line');
			ticks
				.filter((_, i) => i === idx)
				.attr('opacity', 1)
				.transition()
				.duration(700)
				.attr('opacity', 0.25);
		}, 1000);

		return () => {
			clearInterval(interval);
			svg.selectAll('*').remove();
		};
	});

	function formatTime(ts: number): string {
		const d = new Date(ts);
		const hh = String(d.getHours()).padStart(2, '0');
		const mm = String(d.getMinutes()).padStart(2, '0');
		const ss = String(d.getSeconds()).padStart(2, '0');
		return `${hh}:${mm}:${ss}`;
	}

	function eventTimestamp(e: DashboardEvent): number {
		if (e.type === 'ping') return e.data.ts_ms;
		return Date.now();
	}

	function eventSummary(e: DashboardEvent): string {
		switch (e.type) {
			case 'ping':
				return `ts=${e.data.ts_ms}`;
			case 'file_changed':
				return `${e.data.paths.length} file${e.data.paths.length === 1 ? '' : 's'}`;
			case 'index_updated':
				return `+${e.data.changed} / -${e.data.deleted}`;
			case 'reindex_progress':
				return `${e.data.phase} ${e.data.percent}%`;
		}
	}
</script>

{#if (project && !project.indexed) || projectError !== null}
	<EmptyState
		icon={DatabaseZap}
		title="Index not built yet"
		description={projectError ?? 'Run a fresh pass to populate the dashboard.'}
		actionLabel="Reindex"
		onAction={onPulseReindex}
	/>
{/if}

<div class="pulse-grid">
	<Card class="focused-card">
		<CardHeader>
			<CardTitle>Focused file</CardTitle>
		</CardHeader>
		<CardContent>
			<input
				type="text"
				bind:value={focusedPath}
				onkeydown={onPathKey}
				placeholder="src/lib.rs"
				class="w-full rounded border border-[var(--color-fg-muted)] bg-transparent px-2 py-1 font-mono text-sm focus:border-[var(--color-amber)] focus:outline-none"
			/>
			{#if focused}
				<div class="mt-3 font-mono text-sm">
					<div>
						<span class="text-[var(--color-fg-muted)]">{focused.language}</span>
						<span class="text-[var(--color-fg-muted)]"> · </span>
						<span class="text-[var(--color-fg)]">{focused.lines}</span>
						<span class="text-[var(--color-fg-muted)]"> lines · </span>
						<span class="text-[var(--color-fg)]">{focused.symbols.length}</span>
						<span class="text-[var(--color-fg-muted)]"> symbols</span>
					</div>
					<div class="mt-1">
						<span class="text-[var(--color-fg-muted)]">impact: </span>
						<span style="color: var(--color-amber);">{focused.impact.direct}</span>
						<span class="text-[var(--color-fg-muted)]"> direct / </span>
						<span class="text-[var(--color-fg)]">{focused.impact.transitive}</span>
						<span class="text-[var(--color-fg-muted)]"> transitive</span>
					</div>
					{#if focused.symbols.length > 0}
						<ul class="mt-2 max-h-40 overflow-y-auto text-xs text-[var(--color-fg-muted)]">
							{#each focused.symbols.slice(0, 10) as s, si (si)}
								<li>
									<span class="text-[var(--color-fg)]">{s.name}</span>
									<span> ({s.kind}, L{s.line_start})</span>
								</li>
							{/each}
							{#if focused.symbols.length > 10}
								<li>... +{focused.symbols.length - 10} more</li>
							{/if}
						</ul>
					{/if}
				</div>
			{:else if focusedError}
				<div class="mt-2 text-sm" style="color: var(--color-amber);">{focusedError}</div>
			{:else}
				<p class="mt-2 text-xs text-[var(--color-fg-muted)]">type a path and press Enter</p>
			{/if}
			<div class="mt-4 font-mono text-sm">
				{#if project}
					{#if project.indexed}
						<span class="text-[var(--color-fg)]">{project.files}</span>
						<span class="text-[var(--color-fg-muted)]"> files / </span>
						<span class="text-[var(--color-fg)]">{project.symbols}</span>
						<span class="text-[var(--color-fg-muted)]"> symbols</span>
					{:else}
						<span style="color: var(--color-amber);"
							>No index found - run <code>qartez index</code></span
						>
					{/if}
				{:else if projectError}
					<span class="text-[var(--color-fg-muted)]">unable to load project</span>
				{:else}
					<span class="text-[var(--color-fg-muted)]">loading...</span>
				{/if}
			</div>
		</CardContent>
	</Card>

	<Card class="ring-card">
		<CardHeader>
			<CardTitle>Impact ring</CardTitle>
		</CardHeader>
		<CardContent class="flex items-center justify-center">
			<svg bind:this={ringEl} aria-hidden="true"></svg>
		</CardContent>
	</Card>

	<Card class="feed-card">
		<CardHeader>
			<CardTitle>Recent activity</CardTitle>
		</CardHeader>
		<CardContent>
			{#if reindexActive}
				<div
					class="reindex-bar mb-2 h-1 w-full overflow-hidden rounded"
					style="background: color-mix(in srgb, var(--color-amber) 20%, transparent);"
				>
					<div
						class="reindex-bar-fill h-full w-1/3"
						style="background: var(--color-amber); animation: reindex-shimmer 1s linear infinite;"
					></div>
				</div>
			{/if}
			{#if recent.length === 0}
				<p class="font-mono text-sm text-[var(--color-fg-muted)]">Waiting for events...</p>
			{:else}
				<ul class="flex flex-col gap-2">
					{#each recent as evt, i (i)}
						<li class="flex items-center gap-3 font-mono text-xs">
							<span class="w-20 shrink-0 text-[var(--color-fg-muted)]"
								>{formatTime(eventTimestamp(evt))}</span
							>
							<Badge variant="outline" class="min-w-32 justify-center">{evt.type}</Badge>
							<span class="text-[var(--color-fg)]">{eventSummary(evt)}</span>
						</li>
					{/each}
				</ul>
			{/if}
		</CardContent>
	</Card>

	<SymbolPanel
		symbolId={pinnedSymbolId}
		onClose={onClosePinned}
		onPin={onPinSymbol}
		onOpenFile={onOpenSymbolFile}
		onPivot={onPivotSymbol}
	/>
</div>

<style>
	.pulse-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		grid-template-rows: auto 1fr;
		gap: 1rem;
		min-height: 100%;
	}

	:global(.focused-card) {
		grid-column: 1 / 2;
		grid-row: 1 / 2;
	}

	:global(.ring-card) {
		grid-column: 2 / 3;
		grid-row: 1 / 2;
	}

	:global(.feed-card) {
		grid-column: 1 / 3;
		grid-row: 2 / 3;
	}

	@keyframes reindex-shimmer {
		0% {
			transform: translateX(-100%);
		}
		100% {
			transform: translateX(300%);
		}
	}
</style>
