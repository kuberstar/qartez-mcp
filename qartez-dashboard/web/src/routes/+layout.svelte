<script lang="ts">
	import '../app.css';
	import '@fontsource/inter/latin-400.css';
	import '@fontsource/inter/latin-500.css';
	import '@fontsource/inter/latin-600.css';
	import '@fontsource/jetbrains-mono/latin-400.css';
	import favicon from '$lib/assets/favicon.svg';
	import { page } from '$app/state';
	import { dashboardSocket } from '$lib/ws.svelte';
	import { triggerReindex } from '$lib/api';
	import Activity from '@lucide/svelte/icons/activity';
	import Network from '@lucide/svelte/icons/network';
	import Boxes from '@lucide/svelte/icons/boxes';
	import HeartPulse from '@lucide/svelte/icons/heart-pulse';
	import Flame from '@lucide/svelte/icons/flame';
	import AlertTriangle from '@lucide/svelte/icons/triangle-alert';
	import Copy from '@lucide/svelte/icons/copy';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Settings from '@lucide/svelte/icons/settings';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';

	let { children } = $props();

	const VERSION = '0.1.0';
	const sock = dashboardSocket();

	const navItems = [
		{ href: '/', label: 'Project Pulse', icon: Activity },
		{ href: '/map', label: 'Map', icon: Network },
		{ href: '/symbols', label: 'Symbols', icon: Boxes },
		{ href: '/health', label: 'Health', icon: HeartPulse },
		{ href: '/hotspots', label: 'Hotspots', icon: Flame },
		{ href: '/smells', label: 'Smells', icon: AlertTriangle },
		{ href: '/clones', label: 'Clones', icon: Copy },
		{ href: '/dead-code', label: 'Dead Code', icon: Trash2 },
		{ href: '/settings', label: 'Settings', icon: Settings }
	];

	function isActive(href: string): boolean {
		const path = page.url.pathname;
		if (href === '/') return path === '/';
		return path === href || path.startsWith(href + '/');
	}

	const reindexActive = $derived.by(() => {
		for (const evt of sock.events) {
			if (evt.type === 'reindex_progress') {
				return evt.data.phase !== 'complete';
			}
		}
		return false;
	});

	let reindexBusy = $state(false);
	let reindexError = $state<string | null>(null);

	async function onReindexClick(): Promise<void> {
		reindexBusy = true;
		reindexError = null;
		try {
			await triggerReindex();
		} catch (err) {
			reindexError = (err as Error).message;
			setTimeout(() => {
				reindexError = null;
			}, 4000);
		} finally {
			reindexBusy = false;
		}
	}

	const reconnectSeconds = $derived.by(() => {
		if (sock.reconnectIn === null) return null;
		return Math.max(1, Math.ceil(sock.reconnectIn / 1000));
	});
</script>

<svelte:head><link rel="icon" href={favicon} /></svelte:head>

<div class="flex h-screen w-screen flex-col overflow-hidden bg-[var(--color-bg)] text-[var(--color-fg)]">
	<header
		class="flex h-14 shrink-0 items-center justify-between border-b border-[var(--color-border)] bg-[var(--color-surface)] px-6"
	>
		<div class="flex items-center gap-3">
			<span class="qartez-wordmark font-mono text-lg">qartez</span>
			<span
				class="rounded-md border border-[var(--color-border)] bg-[var(--color-elevated)] px-2 py-0.5 font-mono text-xs text-[var(--color-fg-muted)]"
				>v{VERSION}</span
			>
		</div>
		<div class="flex items-center gap-3">
			<button
				type="button"
				class="reindex-btn"
				class:active={reindexActive}
				onclick={onReindexClick}
				disabled={reindexActive || reindexBusy}
				title={reindexError ?? (reindexActive ? 'reindex in progress' : 'trigger a fresh reindex')}
			>
				<RefreshCw size={14} strokeWidth={1.75} class={reindexActive ? 'spin' : ''} />
				<span>{reindexActive ? 'reindexing...' : 'Reindex'}</span>
			</button>
			<div
				class="flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-elevated)] px-3 py-1.5"
			>
				<span
					class="status-dot"
					class:status-dot-on={sock.connected}
					class:status-dot-off={!sock.connected}
					aria-hidden="true"
				></span>
				<span class="font-mono text-xs text-[var(--color-fg-muted)]">
					{#if sock.connected}
						connected
					{:else if reconnectSeconds !== null}
						reconnecting in {reconnectSeconds}s
					{:else}
						reconnecting
					{/if}
				</span>
			</div>
		</div>
	</header>

	<div class="flex min-h-0 flex-1">
		<nav
			class="flex w-60 shrink-0 flex-col gap-1 border-r border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-4"
		>
			{#each navItems as item (item.href)}
				{@const Icon = item.icon}
				{@const active = isActive(item.href)}
				<a
					href={item.href}
					class="nav-link flex items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors"
					class:nav-link-active={active}
					data-sveltekit-preload-data="hover"
				>
					<Icon size={16} strokeWidth={1.75} />
					<span>{item.label}</span>
				</a>
			{/each}
		</nav>

		<main class="min-h-0 flex-1 overflow-auto p-6">
			{@render children()}
		</main>
	</div>
</div>

<style>
	.qartez-wordmark {
		color: var(--color-amber);
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	.status-dot {
		display: inline-block;
		width: 8px;
		height: 8px;
		border-radius: 50%;
	}

	.status-dot-on {
		background: var(--color-success);
		box-shadow: 0 0 0 2px color-mix(in oklch, var(--color-success) 25%, transparent);
	}

	.status-dot-off {
		background: var(--color-amber);
		animation: pulse-amber 1.4s ease-in-out infinite;
	}

	@keyframes pulse-amber {
		0%,
		100% {
			background: var(--color-amber-dim);
			box-shadow: 0 0 0 2px color-mix(in oklch, var(--color-amber) 0%, transparent);
		}
		50% {
			background: var(--color-amber);
			box-shadow: 0 0 0 4px color-mix(in oklch, var(--color-amber) 25%, transparent);
		}
	}

	.nav-link {
		color: var(--color-fg-muted);
	}

	.nav-link:hover {
		background: var(--color-elevated);
		color: var(--color-fg);
	}

	.nav-link-active {
		background: color-mix(in oklch, var(--color-amber) 12%, transparent);
		color: var(--color-amber);
		border-left: 2px solid var(--color-amber);
		padding-left: calc(0.75rem - 2px);
	}

	.nav-link-active:hover {
		background: color-mix(in oklch, var(--color-amber) 18%, transparent);
		color: var(--color-amber);
	}

	.reindex-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-family: 'JetBrains Mono', monospace;
		font-size: 0.75rem;
		padding: 0.35rem 0.7rem;
		background: var(--color-elevated);
		color: var(--color-fg);
		border: 1px solid var(--color-border);
		border-radius: 0.375rem;
		cursor: pointer;
		transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
	}

	.reindex-btn:hover:not(:disabled) {
		background: color-mix(in oklch, var(--color-amber) 12%, transparent);
		border-color: var(--color-amber);
		color: var(--color-amber);
	}

	.reindex-btn:disabled {
		cursor: not-allowed;
		opacity: 0.7;
	}

	.reindex-btn.active {
		color: var(--color-amber);
		border-color: var(--color-amber);
		background: color-mix(in oklch, var(--color-amber) 10%, transparent);
	}

	:global(.reindex-btn .spin) {
		animation: reindex-spin 1.1s linear infinite;
	}

	@keyframes reindex-spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}
</style>
