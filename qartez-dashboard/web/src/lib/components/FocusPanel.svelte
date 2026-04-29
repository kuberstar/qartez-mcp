<script lang="ts">
	import { fetchFocusedFile } from '$lib/api';
	import type { FocusedFile } from '$lib/types';

	type Props = {
		path: string | null;
		onClose: () => void;
		onPin: (path: string) => void;
		onExplore: (path: string) => void;
		showComplexity?: boolean;
	};

	let { path, onClose, onPin, onExplore, showComplexity = false }: Props = $props();

	let panelEl: HTMLDivElement | null = $state(null);
	let data = $state<FocusedFile | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);

	$effect(() => {
		const current = path;
		if (!current) {
			data = null;
			error = null;
			loading = false;
			return;
		}
		let cancelled = false;
		loading = true;
		error = null;
		data = null;
		fetchFocusedFile(current)
			.then((res) => {
				if (cancelled) return;
				data = res;
				loading = false;
			})
			.catch((err) => {
				if (cancelled) return;
				error = (err as Error).message;
				loading = false;
			});
		return () => {
			cancelled = true;
		};
	});

	$effect(() => {
		if (path === null) return;

		function onKey(e: KeyboardEvent) {
			if (e.key === 'Escape') onClose();
		}
		function onPointer(e: PointerEvent) {
			if (panelEl && !panelEl.contains(e.target as Node)) onClose();
		}

		document.addEventListener('keydown', onKey);

		const raf = requestAnimationFrame(() => {
			document.addEventListener('pointerdown', onPointer);
		});

		return () => {
			cancelAnimationFrame(raf);
			document.removeEventListener('keydown', onKey);
			document.removeEventListener('pointerdown', onPointer);
		};
	});

	const visible = $derived(path !== null);
	const topSymbols = $derived(data ? data.symbols.slice(0, 5) : []);
	const topComplex = $derived(
		data
			? [...data.symbols]
					.filter((s) => s.complexity !== null && s.complexity > 0)
					.sort((a, b) => (b.complexity ?? 0) - (a.complexity ?? 0))
					.slice(0, 5)
			: []
	);

	function handlePin() {
		if (!path || loading || error) return;
		onPin(path);
	}

	function handleExplore() {
		if (!path || loading || error) return;
		onExplore(path);
	}
</script>

<div
	bind:this={panelEl}
	class="focus-panel"
	class:visible
	aria-hidden={!visible}
	role="complementary"
>
	{#if path}
		<header class="panel-header">
			<div class="path mono">{path}</div>
			<button
				type="button"
				class="close-btn mono"
				aria-label="Close focus panel"
				onclick={onClose}>X</button
			>
		</header>

		<div class="panel-body">
			{#if loading}
				<div class="state-line mono">loading...</div>
			{:else if error}
				<div class="state-line error mono">{error}</div>
			{:else if data}
				<div class="meta-line mono">
					<span class="meta-strong">{data.language}</span>
					<span class="sep">·</span>
					<span class="meta-strong">{data.lines}</span>
					<span class="meta-muted"> lines</span>
					<span class="sep">·</span>
					<span class="meta-strong">{data.dependents.length}</span>
					<span class="meta-muted"> dependents</span>
					<span class="sep">·</span>
					<span class="meta-strong">{data.impact.transitive}</span>
					<span class="meta-muted"> transitive</span>
				</div>

				<section class="symbols">
					<div class="section-title mono">Top symbols</div>
					{#if topSymbols.length === 0}
						<div class="empty mono">no symbols</div>
					{:else}
						<ul class="symbol-list">
							{#each topSymbols as sym, i (i)}
								<li class="symbol-row mono">
									<span class="kind-badge">{sym.kind}</span>
									<span class="sym-name">{sym.name}</span>
									<span class="sym-lines"
										>L{sym.line_start}{sym.line_start === sym.line_end
											? ''
											: `-${sym.line_end}`}</span
									>
								</li>
							{/each}
						</ul>
					{/if}
				</section>

				{#if showComplexity}
					<section class="symbols">
						<div class="section-title mono">Top complex symbols</div>
						{#if topComplex.length === 0}
							<div class="empty mono">no complexity data</div>
						{:else}
							<ul class="symbol-list">
								{#each topComplex as sym, i (i)}
									<li class="symbol-row with-complexity mono">
										<span class="kind-badge">{sym.kind}</span>
										<span class="sym-name">{sym.name}</span>
										<span class="complexity-badge">cc={sym.complexity}</span>
										<span class="sym-lines"
											>L{sym.line_start}{sym.line_start === sym.line_end
												? ''
												: `-${sym.line_end}`}</span
										>
									</li>
								{/each}
							</ul>
						{/if}
					</section>
				{/if}
			{/if}
		</div>

		<footer class="panel-footer">
			<button
				type="button"
				class="pin-btn mono"
				onclick={handlePin}
				disabled={loading || error !== null}>Pin to /</button
			>
			<button
				type="button"
				class="explore-btn mono"
				onclick={handleExplore}
				disabled={loading || error !== null}>Explore</button
			>
		</footer>
	{/if}
</div>

<style>
	.focus-panel {
		position: absolute;
		top: 0;
		right: 0;
		bottom: 0;
		width: min(420px, 90vw);
		background: var(--color-surface);
		border-left: 1px solid var(--color-border);
		display: flex;
		flex-direction: column;
		z-index: 20;
		transform: translateX(100%);
		transition: transform 200ms ease;
		box-shadow: -2px 0 12px rgba(0, 0, 0, 0.4);
	}

	.focus-panel.visible {
		transform: translateX(0);
	}

	.panel-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0.75rem 0.85rem;
		border-bottom: 1px solid var(--color-border);
	}

	.path {
		font-size: 0.78rem;
		color: var(--color-fg);
		word-break: break-all;
		line-height: 1.35;
		flex: 1;
	}

	.close-btn {
		flex-shrink: 0;
		width: 1.6rem;
		height: 1.6rem;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		background: transparent;
		color: var(--color-fg-muted);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		font-size: 0.75rem;
		cursor: pointer;
		transition:
			color 120ms ease,
			border-color 120ms ease,
			background 120ms ease;
	}

	.close-btn:hover {
		color: var(--color-amber);
		border-color: var(--color-amber);
	}

	.panel-body {
		flex: 1;
		overflow-y: auto;
		padding: 0.85rem;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
	}

	.state-line {
		font-size: 0.78rem;
		color: var(--color-fg-muted);
	}

	.state-line.error {
		color: var(--color-amber);
	}

	.meta-line {
		font-size: 0.78rem;
		line-height: 1.5;
	}

	.meta-strong {
		color: var(--color-fg);
	}

	.meta-muted {
		color: var(--color-fg-muted);
	}

	.sep {
		color: var(--color-fg-muted);
		margin: 0 0.3rem;
	}

	.section-title {
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--color-fg-muted);
		margin-bottom: 0.45rem;
	}

	.symbol-list {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		list-style: none;
		padding: 0;
		margin: 0;
	}

	.symbol-row {
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.75rem;
		padding: 0.3rem 0.4rem;
		background: color-mix(in srgb, var(--color-bg) 60%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
	}

	.symbol-row.with-complexity {
		grid-template-columns: auto 1fr auto auto;
	}

	.complexity-badge {
		font-size: 0.65rem;
		padding: 0.1rem 0.4rem;
		border: 1px solid var(--color-amber);
		border-radius: 0.2rem;
		color: var(--color-amber);
		white-space: nowrap;
	}

	.kind-badge {
		font-size: 0.65rem;
		padding: 0.1rem 0.4rem;
		border: 1px solid var(--color-border);
		border-radius: 0.2rem;
		color: var(--color-fg-muted);
		text-transform: lowercase;
		white-space: nowrap;
	}

	.sym-name {
		color: var(--color-fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.sym-lines {
		color: var(--color-fg-muted);
		white-space: nowrap;
	}

	.empty {
		font-size: 0.75rem;
		color: var(--color-fg-muted);
	}

	.panel-footer {
		display: flex;
		gap: 0.5rem;
		padding: 0.75rem 0.85rem;
		border-top: 1px solid var(--color-border);
	}

	.pin-btn,
	.explore-btn {
		flex: 1;
		padding: 0.55rem 0.75rem;
		background: transparent;
		border-radius: 0.25rem;
		font-size: 0.78rem;
		cursor: pointer;
		transition:
			background 120ms ease,
			color 120ms ease,
			border-color 120ms ease;
	}

	.pin-btn {
		color: var(--color-amber);
		border: 1px solid var(--color-amber);
	}

	.pin-btn:hover:not(:disabled) {
		background: var(--color-amber);
		color: var(--color-bg);
	}

	.explore-btn {
		color: var(--color-fg);
		border: 1px solid var(--color-border);
	}

	.explore-btn:hover:not(:disabled) {
		background: var(--color-fg);
		color: var(--color-bg);
		border-color: var(--color-fg);
	}

	.pin-btn:disabled,
	.explore-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
