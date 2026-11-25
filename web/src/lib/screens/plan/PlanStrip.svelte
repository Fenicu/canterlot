<script lang="ts">
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import SlimStrip from '#lib/ui/SlimStrip.svelte';
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
	<!-- Задача, ради которой полоса появилась: заблокированная красит строку предупреждением. -->
	<div class="strip" data-role="plan-strip" data-tone={strip.tone}>
		<div class="panel">
			<SlimStrip
				{progress}
				count={`${strip.done}/${strip.total}`}
				word={strip.current.word}
				text={strip.current.title}
				wordTone={strip.tone === 'warning' ? 'warning' : 'accent'}
				warning={strip.tone === 'warning'}
				gaugeLabel="Готово задач плана"
				title={strip.text}
				label={`${strip.text}. ${strip.current.word}: #${strip.current.number} ${strip.current.title}`}
				onclick={onopen}
			/>
		</div>
	</div>
{/if}

<style>
	.strip {
		padding: var(--s2) var(--gutter) var(--s0);
	}
	.panel {
		border-radius: var(--radius-sm);
		background: var(--surface);
	}
</style>
