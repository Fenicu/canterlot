<script lang="ts">
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import Gauge from '#lib/ui/Gauge.svelte';
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
	const progress = $derived(strip && strip.total > 0 ? (strip.done / strip.total) * 100 : 0);
</script>

{#if strip}
	<!-- Одна тонкая строка: полоска готовности, счёт, задача, ради которой полоса появилась, и стрелка. -->
	<div class="strip" data-role="plan-strip" data-tone={strip.tone}>
		<button type="button" class="head" title={strip.text} aria-label={`${strip.text}. ${strip.current.word}: #${strip.current.number} ${strip.current.title}`} onclick={onopen}>
			<span class="bar" aria-hidden="true"><Gauge value={progress} tone={strip.tone === 'warning' ? 'warning' : 'normal'} label="Готово задач плана" /></span>
			<span class="count" data-role="plan-strip-count">{strip.done}/{strip.total}</span>
			<span class="summary" data-role="plan-strip-task"><span class="word">{strip.current.word}</span> {strip.current.title}</span>
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
		white-space: nowrap;
		display: flex;
		align-items: center;
		gap: var(--s4);
		min-height: var(--strip-slim);
		padding: var(--s0) var(--s3) var(--s0) var(--s6);
		border-radius: var(--radius-sm);
		background: var(--surface);
		color: var(--secondary);
		font-size: var(--fs-sm);
		line-height: var(--lh-tight);
		text-align: left;
	}
	.bar {
		display: flex;
		flex: none;
	}
	.bar :global([role='meter']) {
		width: var(--s14);
		height: var(--s2);
	}
	.count {
		flex: none;
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.summary {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		color: var(--text);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.word {
		color: var(--accent-link);
	}
	[data-tone='warning'] .word {
		color: var(--warning);
	}
	.chevron {
		display: flex;
		flex: none;
		color: var(--muted);
	}
	.chevron :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	@media (hover: hover) and (pointer: fine) {
		.head:hover {
			background: var(--hover);
		}
	}
</style>
