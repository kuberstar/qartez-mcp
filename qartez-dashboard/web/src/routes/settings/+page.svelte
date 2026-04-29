<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardHeader, CardTitle, CardContent } from '$lib/components/ui/card';
	import { Button } from '$lib/components/ui/button';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { fetchHealth, fetchProject, triggerReindex } from '$lib/api';
	import type { ProjectSummary } from '$lib/types';
	import PlugZap from '@lucide/svelte/icons/plug-zap';

	type ColorScheme = 'auto' | 'light' | 'dark';
	type RenderMode = 'auto' | 'svg' | 'canvas';

	const LS_RENDER_MODE = 'qartez:graph:renderMode';
	const LS_DIFF_AGAINST = 'qartez:map:diffAgainst';
	const LS_COLOR_SCHEME = 'qartez:settings:colorScheme';
	const DEFAULT_DIFF = 'HEAD~10';

	let version = $state<string | null>(null);
	let project = $state<ProjectSummary | null>(null);
	let host = $state<string>('');
	let loadError = $state<string | null>(null);
	let loading = $state(false);

	let stopped = $state(false);
	let stopError = $state<string | null>(null);
	let stopping = $state(false);

	let renderMode = $state<RenderMode>('auto');
	let diffAgainst = $state<string>(DEFAULT_DIFF);
	let colorScheme = $state<ColorScheme>('auto');

	let reindexing = $state(false);
	let reindexMessage = $state<string | null>(null);
	let reindexError = $state<string | null>(null);
	let reindexTimer: ReturnType<typeof setTimeout> | null = null;

	async function loadAll(): Promise<void> {
		loading = true;
		try {
			const [h, p] = await Promise.all([fetchHealth(), fetchProject()]);
			version = h.version;
			project = p;
			loadError = null;
		} catch (err) {
			loadError = (err as Error).message;
		} finally {
			loading = false;
		}
	}

	async function stopDaemon(): Promise<void> {
		stopping = true;
		stopError = null;
		try {
			const r = await fetch('/api/shutdown', {
				method: 'POST',
				credentials: 'same-origin'
			});
			if (!r.ok) throw new Error(`/api/shutdown ${r.status}`);
			stopped = true;
		} catch (err) {
			stopError = (err as Error).message;
		} finally {
			stopping = false;
		}
	}

	async function onReindex(): Promise<void> {
		reindexing = true;
		reindexError = null;
		try {
			const res = await triggerReindex();
			reindexMessage = res.in_progress ? 'Reindex already in progress' : 'Reindex started';
		} catch (err) {
			reindexError = (err as Error).message;
		} finally {
			reindexing = false;
			if (reindexTimer !== null) clearTimeout(reindexTimer);
			reindexTimer = setTimeout(() => {
				reindexMessage = null;
				reindexError = null;
				reindexTimer = null;
			}, 4000);
		}
	}

	function loadPersisted(): void {
		if (typeof localStorage === 'undefined') return;
		try {
			const raw = localStorage.getItem(LS_RENDER_MODE);
			if (raw === 'auto' || raw === 'svg' || raw === 'canvas') renderMode = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_DIFF_AGAINST);
			if (raw !== null && raw.length > 0) diffAgainst = raw;
		} catch {
			/* ignore */
		}
		try {
			const raw = localStorage.getItem(LS_COLOR_SCHEME);
			if (raw === 'auto' || raw === 'light' || raw === 'dark') colorScheme = raw;
		} catch {
			/* ignore */
		}
	}

	function setRenderMode(mode: RenderMode): void {
		renderMode = mode;
		if (typeof localStorage === 'undefined') return;
		try {
			if (mode === 'auto') localStorage.removeItem(LS_RENDER_MODE);
			else localStorage.setItem(LS_RENDER_MODE, mode);
		} catch {
			/* ignore */
		}
	}

	function persistDiff(): void {
		if (typeof localStorage === 'undefined') return;
		const trimmed = diffAgainst.trim();
		try {
			if (trimmed.length === 0) localStorage.removeItem(LS_DIFF_AGAINST);
			else localStorage.setItem(LS_DIFF_AGAINST, trimmed);
		} catch {
			/* ignore */
		}
	}

	function applyColorScheme(mode: ColorScheme): void {
		if (typeof document === 'undefined') return;
		if (mode === 'auto') {
			document.documentElement.removeAttribute('data-color-scheme');
		} else {
			document.documentElement.setAttribute('data-color-scheme', mode);
		}
	}

	function setColorScheme(mode: ColorScheme): void {
		colorScheme = mode;
		applyColorScheme(mode);
		if (typeof localStorage === 'undefined') return;
		try {
			if (mode === 'auto') localStorage.removeItem(LS_COLOR_SCHEME);
			else localStorage.setItem(LS_COLOR_SCHEME, mode);
		} catch {
			/* ignore */
		}
	}

	onMount(() => {
		host = window.location.host;
		loadPersisted();
		applyColorScheme(colorScheme);
		loadAll();
	});
</script>

{#if stopped}
	<div class="flex h-full items-center justify-center">
		<p class="font-mono text-sm text-[var(--color-fg-muted)]">Daemon stopped. Close this tab.</p>
	</div>
{:else if loadError && !project && !version}
	<EmptyState
		icon={PlugZap}
		title="Daemon unreachable"
		description={loadError}
		actionLabel={loading ? 'Retrying...' : 'Retry'}
		onAction={loadAll}
	/>
{:else}
	<div class="flex flex-col gap-4">
		<Card>
			<CardHeader>
				<CardTitle>Daemon</CardTitle>
			</CardHeader>
			<CardContent>
				<dl class="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 font-mono text-sm">
					<dt class="text-[var(--color-fg-muted)]">Version</dt>
					<dd class="text-[var(--color-fg)]">
						{#if version}
							{version}
						{:else if loadError}
							<span style="color: var(--color-amber);">unavailable</span>
						{:else}
							<span class="text-[var(--color-fg-muted)]">loading...</span>
						{/if}
					</dd>

					<dt class="text-[var(--color-fg-muted)]">Project root</dt>
					<dd class="break-all text-[var(--color-fg)]">
						{#if project}
							{project.root}
						{:else if loadError}
							<span style="color: var(--color-amber);">unavailable</span>
						{:else}
							<span class="text-[var(--color-fg-muted)]">loading...</span>
						{/if}
					</dd>

					<dt class="text-[var(--color-fg-muted)]">Indexed</dt>
					<dd>
						{#if project}
							{#if project.indexed}
								<span style="color: var(--color-success);">yes</span>
							{:else}
								<span style="color: var(--color-amber);">no - run qartez index</span>
							{/if}
						{:else}
							<span class="text-[var(--color-fg-muted)]">loading...</span>
						{/if}
					</dd>

					<dt class="text-[var(--color-fg-muted)]">Files</dt>
					<dd class="text-[var(--color-fg)]">
						{#if project}
							{project.files}
						{:else}
							<span class="text-[var(--color-fg-muted)]">-</span>
						{/if}
					</dd>

					<dt class="text-[var(--color-fg-muted)]">Symbols</dt>
					<dd class="text-[var(--color-fg)]">
						{#if project}
							{project.symbols}
						{:else}
							<span class="text-[var(--color-fg-muted)]">-</span>
						{/if}
					</dd>

					<dt class="text-[var(--color-fg-muted)]">Dashboard URL</dt>
					<dd class="text-[var(--color-fg)]">{host}</dd>

					<dt class="text-[var(--color-fg-muted)]">Auth token</dt>
					<dd class="text-[var(--color-fg-muted)]">
						stored in <span class="text-[var(--color-fg)]">~/.qartez/auth.token</span> (HttpOnly
						cookie - not exposed to JS by design)
					</dd>
				</dl>

				{#if loadError}
					<p class="mt-3 font-mono text-xs" style="color: var(--color-amber);">{loadError}</p>
				{/if}
			</CardContent>
		</Card>

		<Card>
			<CardHeader>
				<CardTitle>Render mode</CardTitle>
			</CardHeader>
			<CardContent>
				<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
					Override the auto SVG/canvas threshold used by /map and /symbols.
				</p>
				<div class="seg" role="tablist" aria-label="render mode">
					{#each ['auto', 'svg', 'canvas'] as const as mode (mode)}
						<button
							type="button"
							role="tab"
							class="seg-btn"
							class:active={renderMode === mode}
							aria-selected={renderMode === mode}
							onclick={() => setRenderMode(mode)}
						>
							{mode}
						</button>
					{/each}
				</div>
			</CardContent>
		</Card>

		<Card>
			<CardHeader>
				<CardTitle>Diff target</CardTitle>
			</CardHeader>
			<CardContent>
				<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
					Default git ref the /map diff view compares against.
				</p>
				<input
					type="text"
					bind:value={diffAgainst}
					onblur={persistDiff}
					placeholder={DEFAULT_DIFF}
					class="diff-input mono"
				/>
			</CardContent>
		</Card>

		<Card>
			<CardHeader>
				<CardTitle>Color scheme</CardTitle>
			</CardHeader>
			<CardContent>
				<p class="mb-3 font-mono text-xs text-[var(--color-fg-muted)]">
					Auto follows your OS preference.
				</p>
				<div class="seg" role="tablist" aria-label="color scheme">
					{#each ['auto', 'light', 'dark'] as const as mode (mode)}
						<button
							type="button"
							role="tab"
							class="seg-btn"
							class:active={colorScheme === mode}
							aria-selected={colorScheme === mode}
							onclick={() => setColorScheme(mode)}
						>
							{mode}
						</button>
					{/each}
				</div>
			</CardContent>
		</Card>

		<div class="flex flex-col items-start gap-3">
			<div class="flex flex-wrap items-center gap-3">
				<Button
					variant="outline"
					onclick={onReindex}
					disabled={reindexing}
					class="border-[var(--color-amber)] text-[var(--color-amber)] hover:bg-[color-mix(in_srgb,var(--color-amber)_15%,transparent)] hover:text-[var(--color-amber)]"
				>
					{reindexing ? 'starting...' : 'Force reindex'}
				</Button>
				{#if reindexMessage}
					<span class="font-mono text-xs" style="color: var(--color-success);"
						>{reindexMessage}</span
					>
				{/if}
				{#if reindexError}
					<span class="font-mono text-xs" style="color: var(--color-amber);">{reindexError}</span>
				{/if}
			</div>

			<div class="flex flex-col items-start gap-2">
				<Button
					variant="outline"
					onclick={stopDaemon}
					disabled={stopping}
					class="border-[var(--color-amber)] text-[var(--color-amber)] hover:bg-[color-mix(in_srgb,var(--color-amber)_15%,transparent)] hover:text-[var(--color-amber)]"
				>
					{stopping ? 'stopping...' : 'Stop daemon'}
				</Button>
				{#if stopError}
					<p class="font-mono text-xs" style="color: var(--color-amber);">{stopError}</p>
				{/if}
			</div>
		</div>
	</div>
{/if}

<style>
	.seg {
		display: inline-flex;
		font-family: 'JetBrains Mono', monospace;
		font-size: 0.75rem;
		background: color-mix(in srgb, var(--color-bg) 85%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		overflow: hidden;
	}

	.seg-btn {
		padding: 0.4rem 0.85rem;
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

	.diff-input {
		width: 16rem;
		max-width: 100%;
		padding: 0.4rem 0.6rem;
		background: var(--color-elevated);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		font-size: 0.8rem;
		outline: none;
	}

	.diff-input:focus {
		border-color: var(--color-amber);
	}

	.mono {
		font-family: 'JetBrains Mono', monospace;
	}
</style>
