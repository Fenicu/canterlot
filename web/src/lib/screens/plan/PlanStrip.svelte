<script lang="ts">
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import Icon from '#lib/ui/Icon.svelte';
	import { planStrip } from './planText';

	let { orchestratorId, onopen }: { orchestratorId: string; onopen: () => void } = $props();

	let runtime: Runtime | null = null;
	try {
		runtime = getRuntime();
	} catch {
		runtime = null;
	}

	// Полоса не дублирует план: она появляется, только когда задача ждёт решения владельца или оркестратора.
	const strip = $derived(planStrip(runtime?.list.plan.of(orchestratorId) ?? []));
</script>

{#if strip}
	<div class="strip" data-role="plan-strip" data-tone={strip.tone}>
		<button type="button" class="head" onclick={onopen}>
			<Icon name="list" />
			<span class="summary">{strip.text}</span>
			<span class="chevron"><Icon name="chevron" /></span>
		</button>
	</div>
{/if}

<style>
	.strip {
		display: flex;
		flex-direction: column;
		padding: var(--s2) var(--gutter) var(--s0);
	}
	.head {
		display: flex;
		align-items: center;
		gap: var(--s4);
		min-height: var(--target);
		padding: var(--s0) var(--s6);
		border-radius: var(--radius-pill);
		background: var(--surface);
		color: var(--secondary);
		font-size: var(--fs-sm);
		text-align: left;
	}
	.head :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
		flex: none;
	}
	.summary {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		color: var(--text);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	[data-tone='warning'] .summary,
	[data-tone='warning'] .head :global(svg:first-child) {
		color: var(--warning);
	}
	.chevron {
		display: flex;
		color: var(--muted);
	}
	@media (hover: hover) and (pointer: fine) {
		.head:hover {
			background: var(--hover);
		}
	}
</style>
