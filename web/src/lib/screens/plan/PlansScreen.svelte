<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { browserRuntime } from '#lib/app/browser.js';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import Icon from '#lib/ui/Icon.svelte';
	import SecondaryHeader from '../SecondaryHeader.svelte';
	import { planBadge, planSummary } from './planText';
	import '../secondary.css';

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

	/** Оркестраторы с планом: последний изменённый план первым. */
	const plans = $derived(
		runtime
			? runtime.list.plan.orchestrators().map((id) => {
					const tasks = runtime!.list.plan.of(id);
					return { id, title: runtime!.list.items.get(id)?.title ?? 'Сессия', summary: planSummary(tasks), badge: planBadge(tasks) };
				})
			: []
	);
	const ready = $derived(runtime !== null && (runtime.list.status === 'live' || runtime.list.status === 'degraded' || plans.length > 0));
</script>

<main id="main" tabindex="-1">
	<SecondaryHeader title="Планы" />
	<div class="secondary-body" data-secondary-ready={ready ? '' : undefined}>
		<div class="secondary-content">
			{#if ready && plans.length === 0}
				<p class="note">Планов пока нет. Оркестратор заводит план инструментом plan_add</p>
			{/if}
			<ul class="plans" data-role="plans-list">
				{#each plans as plan (plan.id)}
					<li data-plan-orchestrator={plan.id}>
						<a href={resolve(`/plan/${plan.id}` as '/')}>
							<span class="title">{plan.title}</span>
							{#if plan.badge}<span class="badge" aria-label={plan.badge.label}>{plan.badge.text}</span>{/if}
							<span class="summary">{plan.summary}</span>
							<span class="chevron"><Icon name="chevron" /></span>
						</a>
					</li>
				{/each}
			</ul>
		</div>
	</div>
</main>

<style>
	.plans {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		margin: var(--s0);
		padding: var(--s0);
		list-style: none;
	}
	a {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto auto;
		align-items: center;
		gap: var(--s1) var(--s4);
		min-height: var(--target);
		padding: var(--s5) var(--s6);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
		text-decoration: none;
	}
	.secondary-content a {
		color: var(--text);
	}
	.title {
		min-width: 0;
		overflow-wrap: anywhere;
		font-weight: var(--weight-medium);
	}
	.badge {
		padding: var(--s0) var(--s3);
		border-radius: var(--radius-pill);
		background: var(--chip);
		color: var(--chip-text);
		font-size: var(--fs-sm);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.summary {
		grid-column: 1 / 3;
		min-width: 0;
		color: var(--secondary);
		font-size: var(--fs-sm);
		overflow-wrap: anywhere;
	}
	.chevron {
		grid-column: 3;
		grid-row: 1 / 3;
		display: flex;
		color: var(--muted);
	}
	.chevron :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	@media (hover: hover) and (pointer: fine) {
		a:hover {
			background: var(--hover);
		}
	}
</style>
