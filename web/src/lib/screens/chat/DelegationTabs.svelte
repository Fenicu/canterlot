<script lang="ts">
	import type { Snippet } from 'svelte';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import PlanView from '#lib/screens/plan/PlanView.svelte';
	import { pane } from '#lib/shell/pane.svelte.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Segmented from '#lib/ui/Segmented.svelte';

	let {
		chat,
		stacked = false,
		onnavigate,
		children
	}: {
		chat: ChatStore;
		/** Широкий стол: план и дерево стоят друг под другом в одной прокрутке, без вкладок. */
		stacked?: boolean;
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

{#if stacked}
	{#if hasPlan}
		<section class="part" data-role="pane-plan" aria-labelledby="{name}-plan">
			<h3 class="part-title" id="{name}-plan">План</h3>
			<PlanView orchestratorId={chat.id} {onnavigate} />
		</section>
		<section class="part" data-role="pane-tree" aria-labelledby="{name}-tree">
			<h3 class="part-title" id="{name}-tree">Делегаты</h3>
			{@render children()}
		</section>
	{:else}
		<div class="part" data-role="pane-tree">{@render children()}</div>
	{/if}
{:else}
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
{/if}

<style>
	.tabs {
		flex: none;
		margin-bottom: var(--s4);
	}
	.plan-slot {
		min-height: 0;
	}
	/* Части стопки идут в потоке одна за другой: заголовок делегатов не наезжает на хвост плана. */
	.part {
		flex: none;
		display: flex;
		flex-direction: column;
	}
	.part + .part {
		margin-top: var(--s8);
	}
	.part-title {
		margin: var(--s0) var(--s0) var(--s3);
		padding-inline: var(--s3);
		color: var(--muted);
		font-size: var(--fs-sm);
		font-weight: var(--weight-medium);
	}
</style>
