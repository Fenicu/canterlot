<script lang="ts">
	import type { Snippet } from 'svelte';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import PlanView from '#lib/screens/plan/PlanView.svelte';
	import { pane } from '#lib/shell/pane.svelte.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Segmented from '#lib/ui/Segmented.svelte';

	let {
		chat,
		onnavigate,
		children
	}: {
		chat: ChatStore;
		/** Переход из плана в чат исполнителя. */
		onnavigate?: () => void;
		/** Дерево делегирования: первая вкладка. */
		children: Snippet;
	} = $props();

	let runtime: Runtime | null = null;
	try {
		runtime = getRuntime();
	} catch {
		runtime = null;
	}

	const TABS = [
		{ value: 'tree', label: 'Делегирование' },
		{ value: 'plan', label: 'План' }
	];
	const name = $props.id();
	// Вкладка «План» есть только у оркестратора с задачами плана.
	const hasPlan = $derived(runtime?.list.plan.has(chat.id) ?? false);
	const pick = (value: string) => {
		pane.delegationTab = value === 'plan' ? 'plan' : 'tree';
	};
</script>

{#if hasPlan}
	<div class="tabs" data-role="delegation-tabs">
		<Segmented {name} label="Раздел панели" options={TABS} bind:value={() => pane.delegationTab, pick} />
	</div>
{/if}
{#if hasPlan && pane.delegationTab === 'plan'}
	<div class="plan-slot" data-role="plan-tab">
		<PlanView orchestratorId={chat.id} {onnavigate} />
	</div>
{:else}
	{@render children()}
{/if}

<style>
	.tabs {
		flex: none;
		margin-bottom: var(--s4);
	}
	.plan-slot {
		min-height: 0;
	}
</style>
