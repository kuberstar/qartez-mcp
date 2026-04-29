<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import Copy from '@lucide/svelte/icons/copy';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import { fetchClones } from '$lib/api';
	import type { ClonesResponse } from '$lib/types';

	let data = $state<ClonesResponse | null>(null);
	let loadError = $state<string | null>(null);
	let loading = $state(false);
	let minLines = $state<number>(8);
	let expanded = $state<Set<string>>(new Set());

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			data = await fetchClones(minLines, 200);
			expanded = new Set();
		} catch (err) {
			loadError = (err as Error).message;
		} finally {
			loading = false;
		}
	}

	onMount(load);

	function toggle(hash: string): void {
		const next = new Set(expanded);
		if (next.has(hash)) next.delete(hash);
		else next.add(hash);
		expanded = next;
	}

	function shortHash(h: string): string {
		return h.length > 12 ? h.slice(0, 12) : h;
	}
</script>

<div class="flex flex-col gap-4">
	<Card>
		<CardHeader>
			<CardTitle>Clones</CardTitle>
		</CardHeader>
		<CardContent>
			<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
				Symbol groups sharing an AST shape hash. Useful for finding refactor candidates.
			</p>
			<div class="flex flex-wrap items-center gap-3">
				<label class="font-mono text-xs text-[var(--color-fg-muted)]">
					min lines:
					<input
						type="number"
						min="1"
						max="500"
						class="lines-input mono"
						bind:value={minLines}
						onchange={load}
					/>
				</label>
				<button type="button" class="reload-btn mono" onclick={load} disabled={loading}>
					{loading ? 'loading...' : 'reload'}
				</button>
			</div>
		</CardContent>
	</Card>

	{#if loadError && !data}
		<EmptyState
			icon={Copy}
			title="Could not load clones"
			description={loadError}
			actionLabel={loading ? 'Retrying...' : 'Retry'}
			onAction={load}
		/>
	{:else if data && !data.indexed}
		<EmptyState
			icon={Copy}
			title="Index not built yet"
			description="Run the indexer first - the dashboard reads .qartez/index.db."
		/>
	{:else if data && data.groups.length === 0}
		<EmptyState
			icon={Copy}
			title="No duplicate shapes"
			description={`No clone groups found at min_lines=${minLines}.`}
		/>
	{:else if data}
		<Card>
			<CardContent>
				<table class="clones-table mono">
					<thead>
						<tr>
							<th></th>
							<th class="num">#</th>
							<th class="num">avg lines</th>
							<th>shape hash</th>
						</tr>
					</thead>
					<tbody>
						{#each data.groups as g (g.shape_hash)}
							{@const open = expanded.has(g.shape_hash)}
							<tr class="group-row" onclick={() => toggle(g.shape_hash)}>
								<td class="toggle">
									{#if open}
										<ChevronDown size={14} strokeWidth={1.75} />
									{:else}
										<ChevronRight size={14} strokeWidth={1.75} />
									{/if}
								</td>
								<td class="num">{g.member_count}</td>
								<td class="num">{g.avg_lines.toFixed(1)}</td>
								<td class="hash">{shortHash(g.shape_hash)}</td>
							</tr>
							{#if open}
								<tr class="member-block">
									<td colspan="4">
										<ul class="member-list">
											{#each g.members as m (m.id)}
												<li>
													<span class="kind">{m.kind}</span>
													<span class="name">{m.name}</span>
													<a class="loc" href={`/?path=${encodeURIComponent(m.path)}`}>
														{m.path}:{m.line_start}-{m.line_end}
													</a>
												</li>
											{/each}
										</ul>
									</td>
								</tr>
							{/if}
						{/each}
					</tbody>
				</table>
			</CardContent>
		</Card>
	{/if}
</div>

<style>
	.mono {
		font-family: 'JetBrains Mono', monospace;
	}
	.clones-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.78rem;
	}
	.clones-table th,
	.clones-table td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--color-border);
		text-align: left;
	}
	.clones-table th {
		color: var(--color-fg-muted);
		font-weight: 500;
	}
	.clones-table td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.clones-table td.hash {
		color: var(--color-fg-muted);
	}
	.clones-table .group-row {
		cursor: pointer;
	}
	.clones-table .group-row:hover {
		background: var(--color-elevated);
	}
	.clones-table .toggle {
		width: 1.5rem;
		color: var(--color-fg-muted);
	}
	.member-block td {
		background: color-mix(in srgb, var(--color-elevated) 60%, transparent);
		padding: 0.5rem 1rem 0.7rem 2rem;
	}
	.member-list {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.member-list li {
		display: flex;
		gap: 0.6rem;
		align-items: baseline;
		font-size: 0.78rem;
	}
	.member-list .kind {
		color: var(--color-fg-muted);
		min-width: 4rem;
	}
	.member-list .name {
		color: var(--color-amber);
		min-width: 10rem;
	}
	.member-list .loc {
		color: var(--color-fg);
		text-decoration: none;
	}
	.member-list .loc:hover {
		color: var(--color-amber);
		text-decoration: underline;
	}
	.lines-input {
		width: 5rem;
		padding: 0.25rem 0.4rem;
		background: var(--color-elevated);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		font-size: 0.75rem;
		margin-left: 0.4rem;
	}
	.reload-btn {
		font-size: 0.7rem;
		padding: 0.25rem 0.6rem;
		background: var(--color-elevated);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		cursor: pointer;
	}
	.reload-btn:hover:not(:disabled) {
		color: var(--color-amber);
		border-color: var(--color-amber);
	}
	.reload-btn:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}
</style>
