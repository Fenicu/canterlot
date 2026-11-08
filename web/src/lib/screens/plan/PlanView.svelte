<script lang="ts">
	import { onMount } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { resolve } from '$app/paths';
	import { getPlan } from '#lib/api/client.js';
	import { browserRuntime } from '#lib/app/browser.js';
	import { getRuntime, type Runtime } from '#lib/app/runtime.js';
	import { modelLabel } from '#lib/format/labels.js';
	import type { PlanTask, PlanTaskDetail, PlanTaskState } from '#lib/stores/plan.svelte.js';
	import Icon from '#lib/ui/Icon.svelte';
	import { blockFold } from '#lib/ui/fold.js';
	import {
		STATE_TONE,
		STATE_WORD,
		commitsText,
		executorView,
		filesText,
		groupTasks,
		shortSha,
		taskTitle
	} from './planText';

	let {
		orchestratorId,
		onnavigate
	}: {
		orchestratorId: string;
		/** Переход по ссылке исполнителя или отчёта: лист плана закрывается. */
		onnavigate?: () => void;
	} = $props();

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

	const DETAIL_DEBOUNCE_MS = 400;
	let details = $state<Record<string, PlanTaskDetail>>({});
	const listId = $props.id();

	// Подробности (коммиты, отчёт) считаются хабом при чтении: экран перечитывает их при
	// каждой правке задачи этого плана, но не чаще, чем раз в DETAIL_DEBOUNCE_MS.
	const revision = $derived(runtime?.list.plan.revisionOf(orchestratorId) ?? 0);
	let fetched = false;
	$effect(() => {
		void revision;
		const id = orchestratorId;
		let alive = true;
		const load = () =>
			getPlan(id)
				.then((res) => {
					if (!alive) return;
					const next: Record<string, PlanTaskDetail> = {};
					for (const task of res.data.tasks) next[task.id] = task;
					details = next;
				})
				.catch(() => undefined);
		if (!fetched) {
			fetched = true;
			void load();
			return () => {
				alive = false;
			};
		}
		const timer = setTimeout(load, DETAIL_DEBOUNCE_MS);
		return () => {
			alive = false;
			clearTimeout(timer);
		};
	});

	const tasks = $derived(runtime?.list.plan.of(orchestratorId) ?? []);
	const groups = $derived(groupTasks(tasks));
	const opened = new SvelteSet<PlanTaskState>();
	const expanded = new SvelteSet<string>();

	function groupOpen(state: PlanTaskState, folded: boolean): boolean {
		return folded ? opened.has(state) : !opened.has(state);
	}
	function toggleGroup(state: PlanTaskState): void {
		if (opened.has(state)) opened.delete(state);
		else opened.add(state);
	}
	function toggleTask(id: string): void {
		if (expanded.has(id)) expanded.delete(id);
		else expanded.add(id);
	}

	const executor = (task: PlanTask) =>
		executorView(
			task,
			(id) => runtime?.list.items.get(id),
			(id) => runtime?.list.deleted.has(id) ?? false
		);
	const chatHref = (id: string): string => resolve(`/c/${id}` as '/');
	const reportHref = (id: string, block: string): string => `${chatHref(id)}?block=${encodeURIComponent(block)}`;
</script>

{#snippet taskRow(task: PlanTask)}
	{@const who = executor(task)}
	{@const detail = details[task.id]}
	{@const open = expanded.has(task.id)}
	{@const bodyId = `${listId}-${task.id}`}
	<li class="task" data-plan-task={task.number} data-state={task.state}>
		<button type="button" class="head" aria-expanded={open} aria-controls={bodyId} onclick={() => toggleTask(task.id)}>
			<span class="title">{taskTitle(task)}</span>
			<span class="state" data-tone={STATE_TONE[task.state]} data-role="plan-state">{STATE_WORD[task.state]}</span>
			<span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
		</button>
		{#if who.row || who.word || (detail && detail.commits.length > 0)}
			<p class="meta" data-role="plan-executor">
				{#if who.row}
					<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
					<a href={chatHref(who.row.id)} onclick={() => onnavigate?.()}>{who.row.title}</a>
					<span class="sep">· {modelLabel(who.row.model)}</span>
				{:else if who.word}
					<span>{who.word}</span>
				{/if}
				{#if detail && detail.commits.length > 0}
					<span class="sep" data-role="plan-commits-count">{who.row || who.word ? '· ' : ''}{commitsText(detail.commits.length, detail.commits_truncated)}</span>
				{/if}
			</p>
		{/if}
		{#if open}
			<div class="fold" transition:blockFold>
				<div class="body" id={bodyId} data-role="plan-task-body">
					{#if who.row && who.word}
						<p class="line" data-role="plan-executor-state">Сессия исполнителя <span data-tone={who.tone}>{who.word}</span></p>
					{:else if who.word}
						<p class="line" data-role="plan-executor-state"><span data-tone={who.tone}>{who.word}</span></p>
					{/if}
					{#if task.note}<p class="note">{task.note}</p>{/if}
					{#if task.blocker}
						<p class="blocker" data-role="plan-blocker"><span class="label">Блокер</span> {task.blocker}</p>
					{/if}
					{#if task.findings.length > 0}
						<div class="section">
							<p class="label">Находки</p>
							<ul class="findings">
								{#each task.findings as finding, index (index)}<li>{finding}</li>{/each}
							</ul>
						</div>
					{/if}
					{#if detail && detail.commits.length > 0}
						<div class="section">
							<p class="label">{commitsText(detail.commits.length, detail.commits_truncated)}{detail.files_changed > 0 ? ` · ${filesText(detail.files_changed)}` : ''}</p>
							<ul class="commits" data-role="plan-commits">
								{#each detail.commits as commit (commit.sha)}
									<li><code>{shortSha(commit.sha)}</code> <span>{commit.subject}</span></li>
								{/each}
							</ul>
						</div>
					{/if}
					{#if who.row && detail?.report_block_id}
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
						<a class="report" data-role="plan-report" href={reportHref(who.row.id, detail.report_block_id)} onclick={() => onnavigate?.()}>
							Отчёт
							<Icon name="chevron" />
						</a>
					{/if}
					{#if !task.note && !task.blocker && task.findings.length === 0 && !(detail && detail.commits.length > 0) && !who.word}
						<p class="line">Подробностей пока нет</p>
					{/if}
				</div>
			</div>
		{/if}
	</li>
{/snippet}

<div class="plan" data-plan={orchestratorId}>
	{#if groups.length === 0}
		<p class="empty">В плане пока нет задач</p>
	{/if}
	{#each groups as group (group.state)}
		{@const open = groupOpen(group.state, group.folded)}
		<section class="group" data-plan-group={group.state} aria-label={group.title}>
			{#if group.folded}
				<button type="button" class="group-head toggle" aria-expanded={open} onclick={() => toggleGroup(group.state)}>
					<span class="group-title">{group.title} · {group.tasks.length}</span>
					<span class="chevron" data-open={open ? '' : undefined}><Icon name="chevron" /></span>
				</button>
			{:else}
				<h3 class="group-head"><span class="group-title" data-tone={STATE_TONE[group.state]}>{group.title} · {group.tasks.length}</span></h3>
			{/if}
			{#if open}
				<ul class="tasks">
					{#each group.tasks as task (task.id)}{@render taskRow(task)}{/each}
				</ul>
			{/if}
		</section>
	{/each}
</div>

<style>
	.plan {
		display: flex;
		flex-direction: column;
		gap: var(--s6);
		min-width: 0;
	}
	.empty {
		margin: var(--s0);
		padding: var(--s6) var(--s3);
		color: var(--muted);
		font-size: var(--fs-meta);
	}
	.group {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
	}
	.group-head {
		display: flex;
		align-items: center;
		gap: var(--s3);
		margin: var(--s0);
		padding: var(--s0) var(--s3);
		font-size: var(--fs-sm);
		font-weight: var(--weight-medium);
	}
	.group-head.toggle {
		min-height: var(--target);
		border-radius: var(--radius-pill);
		background: var(--transparent);
		color: var(--muted);
		text-align: left;
	}
	.group-title {
		flex: 1;
		min-width: 0;
		color: var(--secondary);
	}
	.toggle .group-title {
		color: var(--muted);
	}
	.tasks {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		margin: var(--s0);
		padding: var(--s0);
		list-style: none;
	}
	.task {
		display: flex;
		flex-direction: column;
		padding: var(--s1) var(--s0) var(--s3);
		border-radius: var(--radius-card);
		background: var(--surface);
	}
	.head {
		display: flex;
		align-items: center;
		gap: var(--s4);
		min-height: var(--target);
		padding: var(--s2) var(--s6);
		border-radius: var(--radius-card);
		background: var(--transparent);
		color: var(--text);
		text-align: left;
	}
	.title {
		flex: 1;
		min-width: 0;
		overflow-wrap: anywhere;
		font-size: var(--fs-meta);
		font-weight: var(--weight-medium);
	}
	.state {
		flex: none;
		font-size: var(--fs-sm);
		white-space: nowrap;
	}
	.chevron {
		display: flex;
		flex: none;
		color: var(--muted);
		transition: transform var(--duration-fast) var(--ease);
	}
	.chevron :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	.chevron[data-open] {
		transform: rotate(90deg);
	}
	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: var(--s0) var(--s2);
		margin: var(--s0);
		padding: var(--s0) var(--s6);
		color: var(--secondary);
		font-size: var(--fs-sm);
		min-width: 0;
	}
	.meta a {
		min-width: 0;
		overflow-wrap: anywhere;
		color: var(--accent-link);
	}
	.sep {
		color: var(--muted);
	}
	.fold {
		display: grid;
		grid-template-rows: 1fr;
	}
	.body {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		min-height: 0;
		overflow: hidden;
		padding: var(--s3) var(--s6) var(--s0);
	}
	.body p {
		margin: var(--s0);
		overflow-wrap: anywhere;
	}
	.line {
		color: var(--secondary);
		font-size: var(--fs-sm);
	}
	.note {
		color: var(--text);
		font-size: var(--fs-meta);
		white-space: pre-line;
	}
	.blocker {
		color: var(--warning);
		font-size: var(--fs-meta);
	}
	.label {
		color: var(--muted);
		font-size: var(--fs-sm);
		font-weight: var(--weight-medium);
	}
	.blocker .label {
		color: var(--warning);
	}
	.section {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
	}
	.findings,
	.commits {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		margin: var(--s0);
		padding: var(--s0);
		list-style: none;
		color: var(--secondary);
		font-size: var(--fs-sm);
	}
	.findings li {
		overflow-wrap: anywhere;
	}
	.findings li::before {
		content: '— ';
		color: var(--muted);
	}
	.commits li {
		display: flex;
		gap: var(--s3);
		min-width: 0;
	}
	.commits code {
		flex: none;
		color: var(--muted);
		font-family: var(--font-mono);
		font-size: var(--fs-sm);
	}
	.commits span {
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.report {
		display: inline-flex;
		align-self: flex-start;
		align-items: center;
		gap: var(--s2);
		min-height: var(--target);
		padding-inline: var(--s6);
		border-radius: var(--radius-pill);
		background: var(--raised);
		color: var(--text);
		font-size: var(--fs-meta);
		text-decoration: none;
	}
	.report :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
		color: var(--muted);
	}
	@media (hover: hover) and (pointer: fine) {
		.head:hover,
		.group-head.toggle:hover {
			background: var(--hover);
		}
		.report:hover {
			background: var(--hover);
		}
	}
	[data-tone='accent'] {
		color: var(--accent-link);
	}
	[data-tone='blue'] {
		color: var(--blue);
	}
	[data-tone='warning'] {
		color: var(--warning);
	}
	[data-tone='success'] {
		color: var(--success);
	}
	[data-tone='danger'] {
		color: var(--danger);
	}
	[data-tone='muted'] {
		color: var(--muted);
	}
</style>
