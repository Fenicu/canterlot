<script lang="ts">
	import IconButton from '#lib/ui/IconButton.svelte';
	import { onMount, type Component } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import { delegation } from '#lib/api/client.js';
	import { modelLabel } from '#lib/format/labels.js';
	import { buildDelegation, type DelegationNode, subagentVerb } from '#lib/stores/delegation.js';
	import { isMainAgent, toolTitle } from '#lib/timeline/present.js';

	let {
		chat,
		onclose
	}: {
		chat: ChatStore;
		onclose?: () => void;
	} = $props();

	let selectedBlockId = $state<string | null>(null);
	let serverNodes = $state<DelegationNode[]>([]);
	let LaneComponent = $state<Component<{ chat: ChatStore; rootBlockId: string; embedded?: boolean }> | null>(null);

	$effect(() => {
		if (selectedBlockId && !LaneComponent) {
			import('./Lane.svelte').then((m) => {
				LaneComponent = m.default;
			});
		}
	});

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
		if (node.state === 'done' || node.state === 'final' || node.state === 'completed') return 'Готово';
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

<aside class="desktop-lane-panel" aria-label="Панель делегирования">
	<header class="topbar">
		<h2 class="panel-title grow">Делегирование</h2>
		{#if onclose}
			<IconButton icon="close" label="Закрыть боковую панель" onclick={onclose} />
		{/if}
	</header>

	<div class="tree">
		<div class="tree-root">
			{sessionTitle}
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
					<span class="subagent-name">{node.subagent && !isMainAgent(node.subagent.name) ? node.subagent.name : 'Субагент'}</span>
					{#if node.subagent?.model}
						<span class="subagent-model">· {modelLabel(node.subagent.model)}</span>
					{/if}
					{#if node.task}
						<small class="node-task"> · {toolTitle({ toolName: node.tool_name, summary: node.task }).argument}</small>
					{/if}
				</div>
				<span class={getNodeStateClass(node)}>{getNodeVerb(node)}</span>
			</button>
		{/each}
	</div>

	{#if selectedBlockId && LaneComponent}
		<div class="selected-lane">
			<LaneComponent {chat} rootBlockId={selectedBlockId} embedded />
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
		color: var(--text);
	}
	.topbar {
		display: flex;
		align-items: center;
		min-height: var(--target);
		margin: var(--s5) var(--gutter) var(--s3);
		padding-block: var(--s2);
	}
	.topbar > :global(:last-child) {
		margin-right: var(--icon-hang);
	}
	.panel-title {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	.grow {
		flex: 1;
		min-width: 0;
	}
	.tree {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		margin: var(--s0) var(--gutter) var(--s4);
		padding: var(--s4) var(--s3);
		border-radius: var(--radius-card);
		background: var(--surface);
	}
	.tree-root {
		padding: var(--s1) var(--s3);
		font-size: var(--fs-meta);
		font-weight: var(--weight-medium);
	}
	.meta {
		font-size: var(--fs-sm);
		color: var(--muted);
		font-weight: var(--weight-normal);
	}
	.tree-node {
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-height: var(--target);
		padding: var(--s2) var(--s5);
		border-radius: var(--radius-pill);
		background: var(--transparent);
		color: var(--text);
		font-size: var(--fs-meta);
		text-align: left;
	}
	.tree-node.active {
		background: var(--selected);
	}
	.branch {
		color: var(--accent-link);
		font-family: var(--font-mono);
	}
	.node-info {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.subagent-model, .node-task {
		color: var(--muted);
		font-size: var(--fs-sm);
	}
	.accent {
		color: var(--accent-link);
		font-size: var(--fs-sm);
	}
	.warning {
		color: var(--warning);
		font-size: var(--fs-sm);
	}
	.danger {
		color: var(--danger);
		font-size: var(--fs-sm);
	}
	.muted {
		color: var(--muted);
		font-size: var(--fs-sm);
	}
	.selected-lane {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
</style>
