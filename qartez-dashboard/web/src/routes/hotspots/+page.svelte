<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import Flame from '@lucide/svelte/icons/flame';
	import { fetchHotspots } from '$lib/api';
	import type { HotspotItem, HotspotsResponse } from '$lib/types';

	type SortKey = 'score' | 'health' | 'max_cc' | 'avg_cc' | 'churn' | 'pagerank' | 'path';
	type SortDir = 'asc' | 'desc';

	let data = $state<HotspotsResponse | null>(null);
	let loadError = $state<string | null>(null);
	let loading = $state(false);
	let langFilter = $state<string>('all');
	let sortKey = $state<SortKey>('score');
	let sortDir = $state<SortDir>('desc');

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			data = await fetchHotspots(200);
		} catch (err) {
			loadError = (err as Error).message;
		} finally {
			loading = false;
		}
	}

	onMount(load);

	const languages = $derived.by(() => {
		if (!data) return [] as string[];
		const set = new Set<string>();
		for (const item of data.items) set.add(item.language);
		return Array.from(set).sort();
	});

	const filtered = $derived.by(() => {
		if (!data) return [] as HotspotItem[];
		const items = langFilter === 'all'
			? data.items.slice()
			: data.items.filter((i) => i.language === langFilter);
		const dir = sortDir === 'desc' ? -1 : 1;
		items.sort((a, b) => {
			if (sortKey === 'path') return dir * a.path.localeCompare(b.path);
			const av = a[sortKey] as number;
			const bv = b[sortKey] as number;
			return dir * (av - bv);
		});
		return items;
	});

	function setSort(key: SortKey): void {
		if (sortKey === key) {
			sortDir = sortDir === 'desc' ? 'asc' : 'desc';
		} else {
			sortKey = key;
			sortDir = key === 'path' ? 'asc' : 'desc';
		}
	}

	function arrow(key: SortKey): string {
		if (sortKey !== key) return '';
		return sortDir === 'desc' ? ' v' : ' ^';
	}

	function healthClass(health: number): string {
		if (health < 4) return 'sev-critical';
		if (health < 7) return 'sev-medium';
		return 'sev-ok';
	}
</script>

<div class="flex flex-col gap-4">
	<Card>
		<CardHeader>
			<CardTitle>Hotspots</CardTitle>
		</CardHeader>
		<CardContent>
			<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
				Files ranked by max_cc x pagerank x (1 + churn). Health is mean of per-factor scores
				(0-10, 10 = healthiest).
			</p>
			{#if data}
				<div class="flex flex-wrap items-center gap-2">
					<span class="font-mono text-xs text-[var(--color-fg-muted)]">language:</span>
					<button
						type="button"
						class="lang-btn"
						class:active={langFilter === 'all'}
						onclick={() => (langFilter = 'all')}>all</button
					>
					{#each languages as lang (lang)}
						<button
							type="button"
							class="lang-btn"
							class:active={langFilter === lang}
							onclick={() => (langFilter = lang)}
						>
							{lang}
						</button>
					{/each}
				</div>
			{/if}
		</CardContent>
	</Card>

	{#if loadError && !data}
		<EmptyState
			icon={Flame}
			title="Could not load hotspots"
			description={loadError}
			actionLabel={loading ? 'Retrying...' : 'Retry'}
			onAction={load}
		/>
	{:else if data && !data.indexed}
		<EmptyState
			icon={Flame}
			title="Index not built yet"
			description="Run the indexer first - the dashboard reads .qartez/index.db."
		/>
	{:else if data && filtered.length === 0}
		<EmptyState
			icon={Flame}
			title="No hotspots"
			description="No files carry complexity data. Re-index with imperative language sources."
		/>
	{:else if data}
		<Card>
			<CardContent>
				<div class="overflow-x-auto">
					<table class="hotspot-table mono">
						<thead>
							<tr>
								<th class="num"><button onclick={() => setSort('score')}>Score{arrow('score')}</button></th>
								<th class="num"><button onclick={() => setSort('health')}>Health{arrow('health')}</button></th>
								<th><button onclick={() => setSort('path')}>File{arrow('path')}</button></th>
								<th class="lang">Lang</th>
								<th class="num"><button onclick={() => setSort('max_cc')}>MaxCC{arrow('max_cc')}</button></th>
								<th class="num"><button onclick={() => setSort('avg_cc')}>AvgCC{arrow('avg_cc')}</button></th>
								<th class="num"><button onclick={() => setSort('churn')}>Churn{arrow('churn')}</button></th>
								<th class="num"><button onclick={() => setSort('pagerank')}>PR{arrow('pagerank')}</button></th>
							</tr>
						</thead>
						<tbody>
							{#each filtered as item (item.path)}
								<tr>
									<td class="num">{item.score.toFixed(2)}</td>
									<td class="num {healthClass(item.health)}">{item.health.toFixed(1)}</td>
									<td class="path"><a href={`/?path=${encodeURIComponent(item.path)}`}>{item.path}</a></td>
									<td class="lang">{item.language}</td>
									<td class="num">{item.max_cc}</td>
									<td class="num">{item.avg_cc.toFixed(1)}</td>
									<td class="num">{item.churn}</td>
									<td class="num">{item.pagerank.toFixed(4)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</CardContent>
		</Card>
	{/if}
</div>

<style>
	.mono {
		font-family: 'JetBrains Mono', monospace;
	}
	.hotspot-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.78rem;
	}
	.hotspot-table th,
	.hotspot-table td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
		text-align: left;
		white-space: nowrap;
	}
	.hotspot-table th {
		color: var(--color-fg-muted);
		font-weight: 500;
		position: sticky;
		top: 0;
		background: var(--color-surface);
	}
	.hotspot-table th button {
		background: transparent;
		border: 0;
		padding: 0;
		cursor: pointer;
		color: inherit;
		font: inherit;
	}
	.hotspot-table th button:hover {
		color: var(--color-amber);
	}
	.hotspot-table td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.hotspot-table td.path a {
		color: var(--color-fg);
		text-decoration: none;
	}
	.hotspot-table td.path a:hover {
		color: var(--color-amber);
		text-decoration: underline;
	}
	.hotspot-table td.lang,
	.hotspot-table th.lang {
		color: var(--color-fg-muted);
	}
	.sev-critical {
		color: #f87171;
	}
	.sev-medium {
		color: var(--color-amber);
	}
	.sev-ok {
		color: var(--color-success);
	}
	.lang-btn {
		font-family: 'JetBrains Mono', monospace;
		font-size: 0.7rem;
		padding: 0.2rem 0.55rem;
		background: var(--color-elevated);
		color: var(--color-fg-muted);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		cursor: pointer;
	}
	.lang-btn:hover {
		color: var(--color-fg);
	}
	.lang-btn.active {
		color: var(--color-amber);
		border-color: var(--color-amber);
		background: color-mix(in srgb, var(--color-amber) 10%, transparent);
	}
</style>
