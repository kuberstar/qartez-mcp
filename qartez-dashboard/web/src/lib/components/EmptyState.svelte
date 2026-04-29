<script lang="ts">
	import type { Component } from 'svelte';

	type Props = {
		icon?: Component;
		title: string;
		description?: string;
		actionLabel?: string;
		onAction?: () => void;
	};

	let { icon: Icon, title, description, actionLabel, onAction }: Props = $props();
</script>

<div class="empty-state">
	{#if Icon}<Icon size={32} strokeWidth={1.5} class="empty-icon" />{/if}
	<div class="empty-title mono">{title}</div>
	{#if description}<div class="empty-desc mono">{description}</div>{/if}
	{#if actionLabel && onAction}
		<button type="button" class="empty-action mono" onclick={onAction}>{actionLabel}</button>
	{/if}
</div>

<style>
	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		padding: 2rem;
		color: var(--color-fg-muted);
	}
	:global(.empty-icon) {
		color: var(--color-fg-muted);
	}
	.empty-title {
		font-size: 0.95rem;
		color: var(--color-fg);
		font-weight: 600;
	}
	.empty-desc {
		font-size: 0.8rem;
		color: var(--color-fg-muted);
		max-width: 30rem;
		text-align: center;
		line-height: 1.4;
	}
	.empty-action {
		margin-top: 0.5rem;
		padding: 0.4rem 0.9rem;
		background: var(--color-elevated);
		border: 1px solid var(--color-border);
		border-radius: 4px;
		color: var(--color-fg);
		font-size: 0.8rem;
		cursor: pointer;
		transition: background 120ms ease;
	}
	.empty-action:hover {
		background: color-mix(in oklch, var(--color-amber) 15%, transparent);
		border-color: var(--color-amber);
		color: var(--color-amber);
	}
	.mono {
		font-family: 'JetBrains Mono', monospace;
	}
</style>
