<script lang="ts">
	import { fetchFocusedSymbol, fetchSymbolCochanges } from '$lib/api';
	import type { FocusedSymbol, SymbolCochange } from '$lib/types';

	type Props = {
		symbolId: number | null;
		onClose: () => void;
		onPin: (id: number) => void;
		onOpenFile: (filePath: string) => void;
		onPivot: (id: number) => void;
	};

	let { symbolId, onClose, onPin, onOpenFile, onPivot }: Props = $props();

	let panelEl: HTMLDivElement | null = $state(null);
	let data = $state<FocusedSymbol | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let cochanges = $state<SymbolCochange[]>([]);
	let cochangesLoading = $state(false);
	let cochangesError = $state<string | null>(null);

	$effect(() => {
		const current = symbolId;
		if (current === null) {
			data = null;
			error = null;
			loading = false;
			cochanges = [];
			cochangesLoading = false;
			cochangesError = null;
			return;
		}
		let cancelled = false;
		loading = true;
		error = null;
		data = null;
		cochanges = [];
		cochangesLoading = false;
		cochangesError = null;
		fetchFocusedSymbol(current)
			.then((res) => {
				if (cancelled) return;
				data = res;
				loading = false;
				cochangesLoading = true;
				fetchSymbolCochanges(current)
					.then((cres) => {
						if (cancelled) return;
						cochanges = cres.cochanges.slice(0, 5);
						cochangesLoading = false;
					})
					.catch((err) => {
						if (cancelled) return;
						cochangesError = (err as Error).message;
						cochangesLoading = false;
					});
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
		if (symbolId === null) return;

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

	const visible = $derived(symbolId !== null);
	const callers = $derived(data ? data.callers.slice(0, 5) : []);
	const callees = $derived(data ? data.callees.slice(0, 5) : []);

	function handlePin() {
		if (symbolId === null || loading || error) return;
		onPin(symbolId);
	}

	function handleOpenFile() {
		if (!data || loading || error) return;
		onOpenFile(data.file_path);
	}

	function handlePivot() {
		if (symbolId === null || loading || error || !data) return;
		onPivot(symbolId);
	}
</script>

<div
	bind:this={panelEl}
	class="focus-panel"
	class:visible
	aria-hidden={!visible}
	role="complementary"
>
	{#if symbolId !== null}
		<header class="panel-header">
			<div class="symbol-name-large mono">
				{data ? data.name : '...'}
				{#if data}
					<span class="kind-badge">{data.kind}</span>
				{/if}
			</div>
			<button
				type="button"
				class="close-btn mono"
				aria-label="Close symbol panel"
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
					<span class="meta-strong">{data.kind}</span>
					<span class="sep">·</span>
					<span class="meta-strong">{data.file_path}</span>
					<span class="meta-muted">:{data.line_start}-{data.line_end}</span>
					<span class="sep">·</span>
					<span class="meta-muted">pr 0.001</span>
					<span class="sep">·</span>
					<span class="meta-muted">cc </span>
					<span class="meta-strong">{data.complexity ?? 'n/a'}</span>
					<span class="sep">·</span>
					<span class="meta-muted">refs </span>
					<span class="meta-strong">{data.reference_count}</span>
				</div>

				{#if data.signature && data.signature.length > 0}
					<section class="signature">
						<div class="section-title mono">Signature</div>
						<pre class="sig-block mono">{data.signature}</pre>
					</section>
				{/if}

				<section class="symbols">
					<div class="section-title mono">Callers (top 5)</div>
					{#if callers.length === 0}
						<div class="empty mono">no callers</div>
					{:else}
						<ul class="symbol-list">
							{#each callers as n (n.id)}
								<li class="symbol-row mono">
									<span class="kind-badge">{n.kind}</span>
									<span class="sym-name">{n.name}</span>
									<span class="sym-loc">{n.file_path}:{n.line_start}</span>
								</li>
							{/each}
						</ul>
					{/if}
				</section>

				<section class="symbols">
					<div class="section-title mono">Callees (top 5)</div>
					{#if callees.length === 0}
						<div class="empty mono">no callees</div>
					{:else}
						<ul class="symbol-list">
							{#each callees as n (n.id)}
								<li class="symbol-row mono">
									<span class="kind-badge">{n.kind}</span>
									<span class="sym-name">{n.name}</span>
									<span class="sym-loc">{n.file_path}:{n.line_start}</span>
								</li>
							{/each}
						</ul>
					{/if}
				</section>

				<section class="symbols">
					<div class="section-title mono">Ships with</div>
					{#if cochangesLoading}
						<div class="empty mono">loading...</div>
					{:else if cochangesError}
						<div class="empty mono">{cochangesError}</div>
					{:else if cochanges.length === 0}
						<div class="empty mono">no cochange data</div>
					{:else}
						<ul class="symbol-list">
							{#each cochanges as n (n.id)}
								<li class="symbol-row mono">
									<span class="kind-badge">{n.kind}</span>
									<span class="sym-name">{n.name}</span>
									<span class="count-badge">x{n.count}</span>
								</li>
							{/each}
						</ul>
					{/if}
				</section>
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
				class="open-btn mono"
				onclick={handleOpenFile}
				disabled={loading || error !== null || !data}>Open file</button
			>
			<button
				type="button"
				class="pivot-btn mono"
				onclick={handlePivot}
				disabled={loading || error !== null || !data}>Pivot graph</button
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

	.symbol-name-large {
		font-size: 0.95rem;
		color: var(--color-fg);
		font-weight: 600;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex: 1;
		word-break: break-all;
		line-height: 1.35;
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

	.sig-block {
		font-size: 0.7rem;
		color: var(--color-fg);
		background: color-mix(in srgb, var(--color-bg) 60%, transparent);
		border: 1px solid var(--color-border);
		border-radius: 0.25rem;
		padding: 0.4rem 0.5rem;
		margin: 0;
		white-space: pre-wrap;
		word-break: break-word;
		overflow-x: auto;
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

	.kind-badge {
		font-size: 0.65rem;
		padding: 0.1rem 0.4rem;
		border: 1px solid var(--color-border);
		border-radius: 0.2rem;
		color: var(--color-fg-muted);
		text-transform: lowercase;
		white-space: nowrap;
	}

	.count-badge {
		font-size: 0.65rem;
		padding: 0.1rem 0.4rem;
		border: 1px solid var(--color-border);
		border-radius: 0.2rem;
		color: var(--color-amber);
		white-space: nowrap;
	}

	.sym-name {
		color: var(--color-fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.sym-loc {
		color: var(--color-fg-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 12rem;
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
	.open-btn,
	.pivot-btn {
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

	.open-btn {
		color: var(--color-fg);
		border: 1px solid var(--color-border);
	}

	.open-btn:hover:not(:disabled) {
		background: var(--color-fg);
		color: var(--color-bg);
		border-color: var(--color-fg);
	}

	.pivot-btn {
		color: var(--color-amber);
		border: 1px solid var(--color-amber);
	}

	.pivot-btn:hover:not(:disabled) {
		background: var(--color-amber);
		color: var(--color-bg);
	}

	.pin-btn:disabled,
	.open-btn:disabled,
	.pivot-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
