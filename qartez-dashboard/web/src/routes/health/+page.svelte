<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import HeartPulse from '@lucide/svelte/icons/heart-pulse';
	import { fetchProjectHealth } from '$lib/api';
	import type { HealthFile, ProjectHealthResponse, HealthSeverity } from '$lib/types';

	type SeverityFilter = HealthSeverity | 'all';

	let data = $state<ProjectHealthResponse | null>(null);
	let loadError = $state<string | null>(null);
	let loading = $state(false);
	let severity = $state<SeverityFilter>('all');

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			data = await fetchProjectHealth(200);
		} catch (err) {
			loadError = (err as Error).message;
		} finally {
			loading = false;
		}
	}

	onMount(load);

	const filtered = $derived.by(() => {
		if (!data) return [] as HealthFile[];
		return severity === 'all'
			? data.files
			: data.files.filter((f) => f.severity === severity);
	});

	function severityClass(s: HealthSeverity): string {
		switch (s) {
			case 'critical':
				return 'sev-critical';
			case 'medium':
				return 'sev-medium';
			case 'low':
				return 'sev-low';
			default:
				return 'sev-ok';
		}
	}
</script>

<div class="flex flex-col gap-4">
	<Card>
		<CardHeader>
			<CardTitle>Project Health</CardTitle>
		</CardHeader>
		<CardContent>
			<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
				Composite verdict per file: hotspot pressure (max_cc x pagerank x churn) crossed with
				god-function and long-parameter smells. Critical = hotspot AND smells, medium = smells
				without hotspot, low = hotspot without smells.
			</p>
		</CardContent>
	</Card>

	{#if loadError && !data}
		<EmptyState
			icon={HeartPulse}
			title="Could not load health"
			description={loadError}
			actionLabel={loading ? 'Retrying...' : 'Retry'}
			onAction={load}
		/>
	{:else if data && !data.indexed}
		<EmptyState
			icon={HeartPulse}
			title="Index not built yet"
			description="Run the indexer first - the dashboard reads .qartez/index.db."
		/>
	{:else if data}
		<div class="grid grid-cols-1 gap-4 md:grid-cols-4">
			<Card>
				<CardHeader>
					<CardTitle>Avg health</CardTitle>
				</CardHeader>
				<CardContent>
					<div class="metric mono">{data.summary.avg_health.toFixed(1)}</div>
					<div class="metric-sub mono">/ 10</div>
				</CardContent>
			</Card>
			<Card>
				<CardHeader>
					<CardTitle>Critical</CardTitle>
				</CardHeader>
				<CardContent>
					<div class="metric mono sev-critical">{data.summary.critical_count}</div>
					<div class="metric-sub mono">files</div>
				</CardContent>
			</Card>
			<Card>
				<CardHeader>
					<CardTitle>Medium</CardTitle>
				</CardHeader>
				<CardContent>
					<div class="metric mono sev-medium">{data.summary.medium_count}</div>
					<div class="metric-sub mono">files</div>
				</CardContent>
			</Card>
			<Card>
				<CardHeader>
					<CardTitle>Low</CardTitle>
				</CardHeader>
				<CardContent>
					<div class="metric mono sev-low">{data.summary.low_count}</div>
					<div class="metric-sub mono">files</div>
				</CardContent>
			</Card>
		</div>

		<Card>
			<CardContent>
				<div class="mb-3 flex flex-wrap items-center gap-2">
					<span class="font-mono text-xs text-[var(--color-fg-muted)]">severity:</span>
					{#each ['all', 'critical', 'medium', 'low', 'ok'] as const as s (s)}
						<button
							type="button"
							class="seg-btn"
							class:active={severity === s}
							onclick={() => (severity = s)}
						>
							{s}
						</button>
					{/each}
				</div>

				{#if filtered.length === 0}
					<p class="font-mono text-xs text-[var(--color-fg-muted)]">No matching files.</p>
				{:else}
					<div class="overflow-x-auto">
						<table class="health-table mono">
							<thead>
								<tr>
									<th>Severity</th>
									<th>File</th>
									<th class="lang">Lang</th>
									<th class="num">Health</th>
									<th class="num">MaxCC</th>
									<th class="num">Smells</th>
									<th class="num">Churn</th>
									<th class="num">PR</th>
								</tr>
							</thead>
							<tbody>
								{#each filtered as f (f.path)}
									<tr>
										<td class="sev {severityClass(f.severity)}">{f.severity}</td>
										<td class="path">
											<a href={`/?path=${encodeURIComponent(f.path)}`}>{f.path}</a>
										</td>
										<td class="lang">{f.language}</td>
										<td class="num">{f.health.toFixed(1)}</td>
										<td class="num">{f.max_cc}</td>
										<td class="num">{f.smell_count}</td>
										<td class="num">{f.churn}</td>
										<td class="num">{f.pagerank.toFixed(4)}</td>
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
	.metric {
		font-size: 1.6rem;
		font-weight: 500;
		color: var(--color-fg);
		font-variant-numeric: tabular-nums;
	}
	.metric-sub {
		font-size: 0.7rem;
		color: var(--color-fg-muted);
	}
	.health-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.78rem;
	}
	.health-table th,
	.health-table td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
		text-align: left;
		white-space: nowrap;
	}
	.health-table th {
		color: var(--color-fg-muted);
		font-weight: 500;
	}
	.health-table td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.health-table td.lang,
	.health-table th.lang {
		color: var(--color-fg-muted);
	}
	.health-table td.path a {
		color: var(--color-fg);
		text-decoration: none;
	}
	.health-table td.path a:hover {
		color: var(--color-amber);
		text-decoration: underline;
	}
	.health-table td.sev {
		font-weight: 500;
	}
	.sev-critical {
		color: #f87171;
	}
	.sev-medium {
		color: var(--color-amber);
	}
	.sev-low {
		color: #facc15;
	}
	.sev-ok {
		color: var(--color-success);
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
