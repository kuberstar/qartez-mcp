<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import AlertTriangle from '@lucide/svelte/icons/triangle-alert';
	import { fetchSmells } from '$lib/api';
	import type { GodFunction, LongParams, SmellsResponse } from '$lib/types';

	type Severity = 'all' | 'high' | 'med';

	let data = $state<SmellsResponse | null>(null);
	let loadError = $state<string | null>(null);
	let loading = $state(false);
	let langFilter = $state<string>('all');
	let severity = $state<Severity>('all');

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			data = await fetchSmells(500);
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
		for (const item of data.god_functions) set.add(item.language);
		for (const item of data.long_params) set.add(item.language);
		return Array.from(set).sort();
	});

	const filteredGods = $derived.by(() => {
		if (!data) return [] as GodFunction[];
		return data.god_functions.filter((g) => {
			if (langFilter !== 'all' && g.language !== langFilter) return false;
			if (severity === 'high' && g.complexity < 30) return false;
			if (severity === 'med' && (g.complexity < 15 || g.complexity >= 30)) return false;
			return true;
		});
	});

	const filteredLongs = $derived.by(() => {
		if (!data) return [] as LongParams[];
		return data.long_params.filter((l) => {
			if (langFilter !== 'all' && l.language !== langFilter) return false;
			if (severity === 'high' && l.param_count < 8) return false;
			if (severity === 'med' && (l.param_count < 5 || l.param_count >= 8)) return false;
			return true;
		});
	});

	function ccBand(cc: number): 'high' | 'med' | 'low' {
		if (cc >= 30) return 'high';
		if (cc >= 15) return 'med';
		return 'low';
	}
</script>

<div class="flex flex-col gap-4">
	<Card>
		<CardHeader>
			<CardTitle>Code Smells</CardTitle>
		</CardHeader>
		<CardContent>
			<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
				God functions (CC >= 15 and >= 50 lines) and long-parameter signatures (>= 5 params,
				receivers excluded). Same heuristics as qartez_smells.
			</p>
			{#if data}
				<div class="flex flex-wrap items-center gap-3">
					<div class="flex items-center gap-2">
						<span class="font-mono text-xs text-[var(--color-fg-muted)]">severity:</span>
						{#each [{ k: 'all', l: 'all' }, { k: 'med', l: 'medium' }, { k: 'high', l: 'high' }] as opt (opt.k)}
							<button
								type="button"
								class="seg-btn"
								class:active={severity === opt.k}
								onclick={() => (severity = opt.k as Severity)}
							>
								{opt.l}
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
				</div>
			{/if}
		</CardContent>
	</Card>

	{#if loadError && !data}
		<EmptyState
			icon={AlertTriangle}
			title="Could not load smells"
			description={loadError}
			actionLabel={loading ? 'Retrying...' : 'Retry'}
			onAction={load}
		/>
	{:else if data && !data.indexed}
		<EmptyState
			icon={AlertTriangle}
			title="Index not built yet"
			description="Run the indexer first - the dashboard reads .qartez/index.db."
		/>
	{:else if data}
		<Card>
			<CardHeader>
				<CardTitle>God functions ({filteredGods.length})</CardTitle>
			</CardHeader>
			<CardContent>
				{#if filteredGods.length === 0}
					<p class="font-mono text-xs text-[var(--color-fg-muted)]">No matching god functions.</p>
				{:else}
					<div class="overflow-x-auto">
						<table class="smell-table mono">
							<thead>
								<tr>
									<th>Symbol</th>
									<th>File</th>
									<th class="lang">Lang</th>
									<th class="num">CC</th>
									<th class="num">Lines</th>
								</tr>
							</thead>
							<tbody>
								{#each filteredGods as g (g.path + ':' + g.line_start)}
									<tr>
										<td class="sym">{g.name}</td>
										<td class="path">
											<a href={`/?path=${encodeURIComponent(g.path)}`}>{g.path}:{g.line_start}</a>
										</td>
										<td class="lang">{g.language}</td>
										<td class="num cc-{ccBand(g.complexity)}">{g.complexity}</td>
										<td class="num">{g.lines}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}
			</CardContent>
		</Card>

		<Card>
			<CardHeader>
				<CardTitle>Long parameter lists ({filteredLongs.length})</CardTitle>
			</CardHeader>
			<CardContent>
				{#if filteredLongs.length === 0}
					<p class="font-mono text-xs text-[var(--color-fg-muted)]">No matching signatures.</p>
				{:else}
					<div class="overflow-x-auto">
						<table class="smell-table mono">
							<thead>
								<tr>
									<th>Symbol</th>
									<th>File</th>
									<th class="lang">Lang</th>
									<th class="num">Params</th>
									<th>Signature</th>
								</tr>
							</thead>
							<tbody>
								{#each filteredLongs as l (l.path + ':' + l.line_start)}
									<tr>
										<td class="sym">{l.name}</td>
										<td class="path">
											<a href={`/?path=${encodeURIComponent(l.path)}`}>{l.path}:{l.line_start}</a>
										</td>
										<td class="lang">{l.language}</td>
										<td class="num">{l.param_count}</td>
										<td class="sig">{l.signature}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}
			</CardContent>
		</Card>
	{/if}
</div>

<style>
	.mono {
		font-family: 'JetBrains Mono', monospace;
	}
	.smell-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.78rem;
	}
	.smell-table th,
	.smell-table td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
		text-align: left;
		white-space: nowrap;
	}
	.smell-table th {
		color: var(--color-fg-muted);
		font-weight: 500;
	}
	.smell-table td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.smell-table td.lang,
	.smell-table th.lang {
		color: var(--color-fg-muted);
	}
	.smell-table td.path a {
		color: var(--color-fg);
		text-decoration: none;
	}
	.smell-table td.path a:hover {
		color: var(--color-amber);
		text-decoration: underline;
	}
	.smell-table td.sym {
		color: var(--color-amber);
	}
	.smell-table td.sig {
		color: var(--color-fg-muted);
		max-width: 32rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.cc-high {
		color: #f87171;
	}
	.cc-med {
		color: var(--color-amber);
	}
	.cc-low {
		color: var(--color-fg);
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
</style>
