<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { delegation } from '#lib/api/client.js';
	import { modelLabel } from '#lib/format/labels.js';
	import type { Runtime } from '#lib/app/runtime.js';
	import { getRuntime } from '#lib/app/runtime.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import { browserRuntime } from './open.js';
	import { activityLine, type Activity } from '#lib/screens/sessions/activityText.js';
	import type { ListRow } from '#lib/stores/list.svelte.js';
	import { groupDelegation, latestTurnStart, mergeNodes, subagentVerb, type DelegationNode } from '#lib/stores/delegation.js';
	import { isMainAgent, toolTitle } from '#lib/timeline/present.js';
	import Icon from '#lib/ui/Icon.svelte';
	import { liveNodes } from './delegationNodes';

	let {
		chat,
		selected = null,
		onselect,
		href
	}: {
		chat: ChatStore;
		selected?: string | null;
		/** Выбор узла на месте: панель стола раскрывает его линию рядом. */
		onselect?: (blockId: string) => void;
		/** Ссылка на линию узла: на телефоне линия открывается отдельным экраном. */
		href?: (blockId: string) => string;
	} = $props();

	let serverNodes = $state<DelegationNode[]>([]);
	let expanded = $state(false);
	let sessionsExpanded = $state(false);
	const listId = $props.id();

	let runtime = $state<Runtime | null>(null);
	try {
		runtime = getRuntime();
	} catch {
		runtime = null;
	}

	const nodes = $derived(mergeNodes(serverNodes, liveNodes(chat)));
	const groups = $derived(groupDelegation(nodes, latestTurnStart(chat.blocks.values())));
	const sessionTitle = $derived((chat.detail?.title as string) || 'Сессия');
	const olderLabel = $derived(`Завершённые · ${groups.older.length}${groups.interrupted > 0 ? `, прервано ${groups.interrupted}` : ''}`);

	// Делегированные сессии живут в общем списке и обновляются его событиями.
	const sessions = $derived(
		runtime ? [...runtime.list.items.values()].filter((row) => row.parent_id === chat.id) : []
	);
	const sessionDone = (row: ListRow): boolean =>
		row.activity.state === 'stopped' || row.activity.state === 'failed';
	const shownSessions = $derived(
		sessions.filter((row) => !sessionDone(row)).sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id))
	);
	const olderSessions = $derived(
		sessions
			.filter(sessionDone)
			.sort((a, b) => (b.last_activity_at ?? b.created_at).localeCompare(a.last_activity_at ?? a.created_at))
	);
	const sessionsLabel = $derived(`Завершённые · ${olderSessions.length}`);

	onMount(async () => {
		try {
			runtime ??= await browserRuntime();
		} catch {
			// список недоступен: остаются узлы живой ленты
		}
		try {
			const res = await delegation(chat.id);
			if (res.data && Array.isArray(res.data)) serverNodes = res.data as DelegationNode[];
		} catch {
			// остаются узлы из живой ленты
		}
	});

	function verb(node: DelegationNode): string {
		if (node.state === 'waiting') return 'Ждёт';
		if (node.state === 'done' || node.state === 'final' || node.state === 'completed') return 'Готово';
		if (node.state === 'failed' || node.state === 'error') return 'Ошибка';
		if (node.state === 'aborted' || node.state === 'stopped' || node.state === 'incomplete') return 'Прервано';
		if (node.tool_name && node.tool_name !== 'Agent' && node.tool_name !== 'Task') return subagentVerb(node.tool_name);
		return 'Работает';
	}

	function tone(node: DelegationNode): string {
		const text = verb(node);
		if (text === 'Ждёт') return 'warning';
		if (text === 'Готово') return 'muted';
		if (text === 'Прервано' || text === 'Ошибка') return 'danger';
		return 'accent';
	}

	function sessionLine(row: ListRow): { text: string; tone: string } {
		const line = activityLine(row.activity as Activity, { now: Date.now() });
		return { text: line.text, tone: line.tone === 'default' ? 'muted' : line.tone };
	}
</script>

{#snippet row(node: DelegationNode)}
	{#snippet body()}
		<span class="branch">└</span>
		<span class="grow node-info">
			<span class="subagent-name">{node.subagent && !isMainAgent(node.subagent.name) ? node.subagent.name : 'Субагент'}</span>
			{#if node.subagent?.model}
				<span class="subagent-model">· {modelLabel(node.subagent.model)}</span>
			{/if}
			{#if node.task}
				<small class="node-task"> · {toolTitle({ toolName: node.tool_name, summary: node.task }).argument}</small>
			{/if}
		</span>
		<span class={tone(node)}>{verb(node)}</span>
	{/snippet}
	{#if href}
		<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
		<a class="tree-node" href={href(node.block_id)}>{@render body()}</a>
	{:else}
		<button type="button" class="tree-node" class:active={selected === node.block_id} onclick={() => onselect?.(node.block_id)}>{@render body()}</button>
	{/if}
{/snippet}

{#snippet sessionRow(row: ListRow)}
	<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
	<a class="tree-node" href={resolve(`/c/${row.id}` as '/')}>
		<span class="branch">└</span>
		<span class="grow node-info">
			<span class="session-name">{row.title}</span>
			<span class="subagent-model">· {modelLabel(row.model)}</span>
		</span>
		<span class={sessionLine(row).tone}>{sessionLine(row).text}</span>
	</a>
{/snippet}

<div class="tree">
	<div class="tree-root">{sessionTitle}</div>

	{#each groups.shown as node (node.block_id)}
		{@render row(node)}
	{/each}

	{#if groups.older.length > 0}
		<button type="button" class="tree-node older-toggle" aria-expanded={expanded} aria-controls={listId} onclick={() => (expanded = !expanded)}>
			<span class="grow" class:danger={groups.interrupted > 0}>{olderLabel}</span>
			<span class="chevron"><Icon name="chevron" /></span>
		</button>
		{#if expanded}
			<div class="older" id={listId}>
				{#each groups.older as node (node.block_id)}
					{@render row(node)}
				{/each}
			</div>
		{/if}
	{/if}

	{#if sessions.length > 0}
		<div class="tree-root sessions-title">Сессии</div>
		{#each shownSessions as row (row.id)}
			{@render sessionRow(row)}
		{/each}
		{#if olderSessions.length > 0}
			<button type="button" class="tree-node older-toggle" aria-expanded={sessionsExpanded} onclick={() => (sessionsExpanded = !sessionsExpanded)}>
				<span class="grow">{sessionsLabel}</span>
				<span class="chevron"><Icon name="chevron" /></span>
			</button>
			{#if sessionsExpanded}
				<div class="older">
					{#each olderSessions as row (row.id)}
						{@render sessionRow(row)}
					{/each}
				</div>
			{/if}
		{/if}
	{/if}
</div>

<style>
	.tree {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		padding: var(--s4) var(--s3);
		border-radius: var(--radius-card);
		background: var(--surface);
	}
	.tree-root {
		padding: var(--s1) var(--s3);
		font-size: var(--fs-meta);
		font-weight: var(--weight-medium);
	}
	.sessions-title {
		margin-top: var(--s2);
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
		text-decoration: none;
	}
	.tree-node.active {
		background: var(--selected);
	}
	.older-toggle {
		color: var(--muted);
		font-size: var(--fs-sm);
	}
	.older-toggle .danger {
		font-size: var(--fs-sm);
	}
	.chevron {
		display: flex;
		color: var(--muted);
		transition: transform var(--duration-fast) var(--ease);
	}
	.chevron :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	.older-toggle[aria-expanded='true'] .chevron {
		transform: rotate(90deg);
	}
	.older {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		animation: older-enter var(--duration-enter) var(--ease-enter);
	}
	@keyframes older-enter {
		from {
			opacity: 0;
			transform: translateY(calc(-1 * var(--shift-enter)));
		}
		to {
			opacity: 1;
			transform: none;
		}
	}
	.grow {
		flex: 1;
		min-width: 0;
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
	.subagent-model,
	.node-task {
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
</style>
