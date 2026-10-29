<script lang="ts">
	import { onMount } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import { delegation } from '#lib/api/client.js';
	import { buildDelegation, type DelegationNode, subagentVerb } from '#lib/stores/delegation.js';
	import Lane from './Lane.svelte';

	let {
		chat,
		onclose
	}: {
		chat: ChatStore;
		onclose?: () => void;
	} = $props();

	let selectedBlockId = $state<string | null>(null);
	let serverNodes = $state<DelegationNode[]>([]);

	const liveNodes = $derived(buildDelegation(chat.blocks.values()));

	const combinedNodes = $derived.by(() => {
		const map = new SvelteMap<string, DelegationNode>();
		for (const n of serverNodes) map.set(n.block_id, n);
		for (const n of liveNodes) map.set(n.block_id, n);
		return [...map.values()];
	});

	const sessionTitle = $derived((chat.detail?.title as string) || 'Сессия');

	onMount(async () => {
		try {
			const res = await delegation(chat.id);
			if (res.data && Array.isArray(res.data)) {
				serverNodes = res.data as DelegationNode[];
			}
		} catch {
			// fallback to liveNodes
		}
	});

	function getNodeVerb(node: DelegationNode): string {
		if (node.state === 'waiting') return 'Ждёт';
		if (node.state === 'final' || node.state === 'completed') return 'Готово';
		if (node.state === 'aborted' || node.state === 'failed' || node.state === 'error') return 'Прервано';
		if (node.tool_name) return subagentVerb(node.tool_name);
		return 'Работает';
	}

	function getNodeStateClass(node: DelegationNode): string {
		const verb = getNodeVerb(node);
		if (verb === 'Ждёт') return 'warning';
		if (verb === 'Готово') return 'muted';
		if (verb === 'Прервано') return 'danger';
		return 'accent';
	}
</script>

<aside class="desktop-lane-panel" aria-label="Панель поручений">
	<header class="topbar">
		<h2 class="panel-title grow">Делегирование</h2>
		{#if onclose}
			<button
				type="button"
				class="icon-button symbol"
				aria-label="Закрыть боковую панель"
				onclick={onclose}
			>
				×
			</button>
		{/if}
	</header>

	<div class="tree">
		<div class="tree-root">
			{sessionTitle} <span class="meta">· оркестратор</span>
		</div>

		{#each combinedNodes as node (node.block_id)}
			<button
				type="button"
				class="tree-node"
				class:active={selectedBlockId === node.block_id}
				onclick={() => {
					selectedBlockId = selectedBlockId === node.block_id ? null : node.block_id;
				}}
			>
				<span class="branch">└</span>
				<div class="grow node-info">
					<span class="subagent-name">{node.subagent?.name || 'Субагент'}</span>
					{#if node.subagent?.model}
						<span class="subagent-model">· {node.subagent.model}</span>
					{/if}
					{#if node.task}
						<small class="node-task"> · {node.task}</small>
					{/if}
				</div>
				<span class={getNodeStateClass(node)}>{getNodeVerb(node)}</span>
			</button>
		{/each}
	</div>

	{#if selectedBlockId}
		<div class="selected-lane">
			<Lane {chat} rootBlockId={selectedBlockId} />
		</div>
	{/if}
</aside>

<style>
	.desktop-lane-panel {
		width: 100%;
		height: 100%;
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		background: var(--surface);
		color: var(--text);
	}
	.topbar {
		display: flex;
		align-items: center;
		padding: var(--s2) var(--s4);
		border-bottom: var(--hairline) solid var(--line);
		min-height: var(--target);
	}
	.panel-title {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	.grow {
		flex: 1;
	}
	.icon-button {
		background: var(--transparent);
		border: none;
		color: var(--text);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		width: var(--target);
		height: var(--target);
		font-size: var(--fs-title);
		border-radius: var(--radius-card);
	}
	.tree {
		padding: var(--s3);
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		border-bottom: var(--hairline) solid var(--line);
	}
	.tree-root {
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
		padding: var(--s1) var(--s2);
	}
	.meta {
		font-size: var(--fs-meta);
		color: var(--muted);
		font-weight: normal;
	}
	.tree-node {
		display: flex;
		align-items: center;
		gap: var(--s2);
		padding: var(--s2);
		border-radius: var(--radius-card);
		background: var(--surface);
		border: var(--hairline) solid transparent;
		color: var(--text);
		text-align: left;
		cursor: pointer;
		min-height: var(--target);
	}
	.tree-node:hover {
		background: var(--raised);
	}
	.tree-node.active {
		background: var(--raised);
		border-color: var(--line);
	}
	.branch {
		color: var(--muted);
		font-family: var(--font-mono);
	}
	.node-info {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.subagent-name {
		font-weight: var(--weight-semibold);
	}
	.subagent-model {
		color: var(--muted);
		font-size: var(--fs-meta);
	}
	.node-task {
		color: var(--muted);
	}
	.accent {
		color: var(--accent);
		font-size: var(--fs-meta);
	}
	.warning {
		color: var(--warning);
		font-size: var(--fs-meta);
	}
	.danger {
		color: var(--danger);
		font-size: var(--fs-meta);
	}
	.muted {
		color: var(--muted);
		font-size: var(--fs-meta);
	}
	.selected-lane {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		border-top: var(--hairline) solid var(--line);
	}
</style>
