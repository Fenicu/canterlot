<script lang="ts">
	import IconButton from '#lib/ui/IconButton.svelte';
	import { type Component } from 'svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import DelegationTabs from './DelegationTabs.svelte';
	import DelegationTree from './DelegationTree.svelte';
	import { pane } from '#lib/shell/pane.svelte.js';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';

	let {
		chat,
		onclose
	}: {
		chat: ChatStore;
		onclose?: () => void;
	} = $props();

	let selectedBlockId = $state<string | null>(null);
	let runtime: Runtime | null = null;
	try {
		runtime = getRuntime();
	} catch {
		runtime = null;
	}
	const planOpen = $derived(pane.delegationTab === 'plan' && (runtime?.list.plan.has(chat.id) ?? false));
	let LaneComponent = $state<Component<{ chat: ChatStore; rootBlockId: string; embedded?: boolean }> | null>(null);

	$effect(() => {
		if (selectedBlockId && !LaneComponent) {
			import('./Lane.svelte').then((m) => {
				LaneComponent = m.default;
			});
		}
	});
</script>

<aside class="desktop-lane-panel" aria-label="Панель делегирования">
	<header class="topbar">
		<h2 class="panel-title grow">Делегирование</h2>
		{#if onclose}
			<IconButton icon="close" label="Закрыть боковую панель" onclick={onclose} />
		{/if}
	</header>

	<div class="tree-slot" class:plan-open={planOpen}>
		<DelegationTabs {chat}>
			<DelegationTree {chat} selected={selectedBlockId} onselect={(id) => (selectedBlockId = selectedBlockId === id ? null : id)} />
		</DelegationTabs>
	</div>

	{#if selectedBlockId && LaneComponent && !planOpen}
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
	.tree-slot {
		flex: none;
		max-height: 50%;
		overflow-y: auto;
		border-radius: var(--radius-card);
		margin: var(--s0) var(--gutter) var(--s4);
	}
	/* План занимает всю высоту панели: линии субагента рядом с ним нет. */
	.tree-slot.plan-open {
		flex: 1;
		max-height: none;
		min-height: 0;
	}
	.selected-lane {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
</style>
