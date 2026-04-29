<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { fetchDeadCode } from '$lib/api';
	import type { DeadCodeItem, DeadCodeResponse } from '$lib/types';

	let data = $state<DeadCodeResponse | null>(null);
	let loadError = $state<string | null>(null);
	let loading = $state(false);
	let kindFilter = $state<string>('all');
	let langFilter = $state<string>('all');
	let pathFilter = $state<string>('');

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			data = await fetchDeadCode(2000);
		} catch (err) {
			loadError = (err as Error).message;
		} finally {
			loading = false;
		}
	}

	onMount(load);

	const kinds = $derived.by(() => {
		if (!data) return [] as string[];
		const set = new Set<string>();
		for (const item of data.items) set.add(item.kind);
		return Array.from(set).sort();
	});

	const languages = $derived.by(() => {
		if (!data) return [] as string[];
		const set = new Set<string>();
		for (const item of data.items) set.add(item.language);
		return Array.from(set).sort();
	});

	const filtered = $derived.by(() => {
		if (!data) return [] as DeadCodeItem[];
		const term = pathFilter.trim().toLowerCase();
		return data.items.filter((item) => {
			if (kindFilter !== 'all' && item.kind !== kindFilter) return false;
			if (langFilter !== 'all' && item.language !== langFilter) return false;
			if (term.length > 0 && !item.path.toLowerCase().includes(term)) return false;
			return true;
		});
	});
</script>

<div class="flex flex-col gap-4">
	<Card>
		<CardHeader>
			<CardTitle>Dead Code</CardTitle>
		</CardHeader>
		<CardContent>
			<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
				Exported symbols with no detected importers and no in-repo references. Read-only -
				delete from your editor after manual review.
			</p>
			{#if data && data.available}
				<div class="flex flex-wrap items-center gap-3">
					<div class="flex items-center gap-2">
						<span class="font-mono text-xs text-[var(--color-fg-muted)]">kind:</span>
						<button
							type="button"
							class="seg-btn"
							class:active={kindFilter === 'all'}
							onclick={() => (kindFilter = 'all')}
						>
							all
						</button>
						{#each kinds as k (k)}
							<button
								type="button"
								class="seg-btn"
								class:active={kindFilter === k}
								onclick={() => (kindFilter = k)}
							>
								{k}
							</button>
						{/each}
					</div>
					<div class="flex flex-wrap items-center gap-2">
						<span class="font-mono text-xs text-[var(--color-fg-muted)]">language:</span>
						<button
							type="button"
							class="seg-btn"
							class:active={langFilter === 'all'}
							onclick={() => (langFilter = 'all')}
						>
							all
						</button>
						{#each languages as lang (lang)}
							<button
								type="button"
								class="seg-btn"
								class:active={langFilter === lang}
								onclick={() => (langFilter = lang)}
							>
								{lang}
							</button>
						{/each}
					</div>
					<input
						type="text"
						class="path-input mono"
						placeholder="filter by path..."
						bind:value={pathFilter}
					/>
				</div>
			{/if}
		</CardContent>
	</Card>

	{#if loadError && !data}
		<EmptyState
			icon={Trash2}
			title="Could not load dead code"
			description={loadError}
			actionLabel={loading ? 'Retrying...' : 'Retry'}
			onAction={load}
		/>
	{:else if data && !data.indexed}
		<EmptyState
			icon={Trash2}
			title="Index not built yet"
			description="Run the indexer first - the dashboard reads .qartez/index.db."
		/>
	{:else if data && !data.available}
		<EmptyState
			icon={Trash2}
			title="Dead code data unavailable"
			description="The unused_exports table is missing. Re-index the project to populate it."
		/>
	{:else if data && filtered.length === 0}
		<EmptyState
			icon={Trash2}
			title="Nothing dead"
			description="No exported symbols match the current filters."
		/>
	{:else if data}
		<Card>
			<CardContent>
				<p class="mb-2 font-mono text-xs text-[var(--color-fg-muted)]">
					{filtered.length} of {data.items.length} symbols
				</p>
				<div class="overflow-x-auto">
					<table class="dead-table mono">
						<thead>
							<tr>
								<th>Symbol</th>
								<th>Kind</th>
								<th>File</th>
								<th class="lang">Lang</th>
								<th class="num">Line</th>
							</tr>
						</thead>
						<tbody>
							{#each filtered as item (item.id)}
								<tr>
									<td class="sym">{item.name}</td>
									<td class="kind">{item.kind}</td>
									<td class="path">
										<a href={`/?path=${encodeURIComponent(item.path)}`}>{item.path}</a>
									</td>
									<td class="lang">{item.language}</td>
									<td class="num">{item.line_start}</td>
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
	.dead-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.78rem;
	}
	.dead-table th,
	.dead-table td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
		text-align: left;
		white-space: nowrap;
	}
	.dead-table th {
		color: var(--color-fg-muted);
		font-weight: 500;
	}
	.dead-table td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.dead-table td.lang,
	.dead-table th.lang,
	.dead-table td.kind {
		color: var(--color-fg-muted);
	}
	.dead-table td.path a {
		color: var(--color-fg);
		text-decoration: none;
	}
	.dead-table td.path a:hover {
		color: var(--color-amber);
		text-decoration: underline;
	}
	.dead-table td.sym {
		color: var(--color-amber);
	}
	.seg-btn {
		font-family: 'JetBrains Mono', monospace;
		font-size: 0.7rem;
		padding: 0.2rem 0.55rem;
		background: var(--color-elevated);
		color: var(--color-fg-muted);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		cursor: pointer;
	}
	.seg-btn:hover {
		color: var(--color-fg);
	}
	.seg-btn.active {
		color: var(--color-amber);
		border-color: var(--color-amber);
		background: color-mix(in srgb, var(--color-amber) 10%, transparent);
	}
	.path-input {
		padding: 0.25rem 0.5rem;
		background: var(--color-elevated);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		font-size: 0.75rem;
		min-width: 14rem;
	}
	.path-input:focus {
		outline: none;
		border-color: var(--color-amber);
	}
</style>
