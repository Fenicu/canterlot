<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { browserRuntime } from '#lib/app/browser.js';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import SecondaryHeader from '../SecondaryHeader.svelte';
	import PlanView from './PlanView.svelte';
	import { planSummary } from './planText';
	import '../secondary.css';

	let { orchestratorId }: { orchestratorId: string } = $props();

	let runtime = $state<Runtime | null>(null);
	try {
		runtime = getRuntime();
	} catch {
		runtime = null;
	}
	onMount(() => {
		if (!runtime)
			browserRuntime()
				.then((value) => (runtime = value))
				.catch(() => undefined);
	});

	const row = $derived(runtime?.list.items.get(orchestratorId) ?? null);
	const gone = $derived(runtime?.list.deleted.has(orchestratorId) ?? false);
	const summary = $derived(runtime ? planSummary(runtime.list.plan.of(orchestratorId)) : '');
	const ready = $derived(runtime !== null && (runtime.list.status === 'live' || runtime.list.status === 'degraded' || row !== null));
</script>

<main id="main" tabindex="-1">
	<SecondaryHeader title="План" subtitle={row?.title} back={{ path: '/plan', label: 'К планам' }} />
	<div class="secondary-body" data-secondary-ready={ready ? '' : undefined}>
		<div class="secondary-content">
			{#if gone}
				<p class="note" data-role="plan-gone">Сессия оркестратора удалена, её плана больше нет</p>
			{:else}
				<div class="lead">
					{#if summary}<p class="note">{summary}</p>{/if}
					{#if row}<a class="chat" href={resolve(`/c/${orchestratorId}` as '/')}>Открыть чат</a>{/if}
				</div>
				<PlanView {orchestratorId} />
			{/if}
		</div>
	</div>
</main>

<style>
	.lead {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--s2) var(--s4);
		margin-bottom: var(--s6);
	}
	.lead .note {
		margin: var(--s0);
	}
	.chat {
		display: inline-flex;
		align-items: center;
		min-height: var(--target);
		font-size: var(--fs-meta);
	}
</style>
