<script lang="ts">
	import { resolve } from '$app/paths';
	import { untrack } from 'svelte';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import { blockLinks } from '#lib/timeline/rows.js';
	import Timeline from '#lib/timeline/Timeline.svelte';
	import { laneModel } from './lane';
	import { LaneFeed } from './laneFeed.svelte.js';
	import TodoPanel from './TodoPanel.svelte';
	import { theme } from '#lib/theme.js';
	import Icon from '#lib/ui/Icon.svelte';

	let {
		chat,
		rootBlockId,
		now = Date.now(),
		embedded = false,
		onInfo
	}: {
		chat: ChatStore;
		rootBlockId: string;
		now?: number;
		embedded?: boolean;
		onInfo?: (runId: string) => void;
	} = $props();

	const blocks = $derived([...chat.blocks.values()]);
	const machineName = $derived((chat.detail?.machine as { name?: string })?.name || 'workstation');

	const feed = $derived(new LaneFeed(chat, rootBlockId));
	$effect(() => {
		const current = feed;
		untrack(() => void current.start());
	});
	const links = $derived(blockLinks(feed.entries, chat.backgroundFacts));
	const working = $derived(chat.activity?.state === 'working' || chat.activity?.state === 'waiting_for_user');
	const model = $derived(
		laneModel(blocks, rootBlockId, {
			machineName,
			children: links.children(rootBlockId),
			harness: chat.detail?.harness as { title?: string; provider?: string } | undefined,
			result: links.result(rootBlockId),
			background: links.background(rootBlockId),
			working,
			load: feed.load
		})
	);
</script>

<div class="lane-screen" class:embedded data-ready>
	{#if !embedded}
		<header class="topbar">
			<a
				class="icon-button"
				href={resolve(`c/${chat.id}`)}
				aria-label="Назад к сессии"
			><Icon name="back" /></a>

			<div class="header-info">
				<div class="title-row">
					<span class="mark" class:running={model.running}></span>
					<h1 class="header-title">{model.title}</h1>
				</div>
				<div class="subtitle">{model.subtitle}</div>
			</div>
			<button
				type="button"
				class="icon-button"
				data-theme-toggle
				aria-label="Сменить тему"
				onclick={() => theme.toggle()}
			>
				<Icon name={theme.effective === 'dark' ? 'sun' : 'moon'} />
			</button>
		</header>
	{/if}

	<nav class="breadcrumb" aria-label="Путь делегирования">
		<p class="path"><a href={resolve(`c/${chat.id}`)}>{model.breadcrumb[0]}</a><span aria-hidden="true">→</span><span>{model.breadcrumb[1]}</span></p>
		<p class="live-line">
			<span class="live-dot" class:active={model.running}></span>
			{model.status}
		</p>
	</nav>

	{#if model.task}
		<div class="lane-intro">
			<strong>Задача</strong>
			<p>{model.task}</p>
		</div>
	{/if}

	<div class="lane-timeline-container">
		{#key rootBlockId}
			<Timeline
				chat={chat}
				onInfo={onInfo}
				now={now}
				laneRoot={rootBlockId}
				{feed}
			/>
		{/key}
	</div>

	<TodoPanel {chat} lane={rootBlockId} />

	<footer class="lane-footer">
		{#if feed.load === 'failed'}<button type="button" class="return-link" onclick={() => feed.start()}>Повторить загрузку</button>{/if}
		{#if model.running}<p class="pending-line">Результат вернётся в основную сессию</p>{/if}
		{#if !embedded}
			<a class="return-link" href={resolve(`c/${chat.id}`)}>К основной сессии</a>
		{/if}
	</footer>
</div>

<style>
	.lane-screen {
		flex: 1;
		height: 100%;
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		position: relative;
		color: var(--text);
	}
	.topbar {
		display: flex;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
		min-height: var(--target);
		margin: var(--s5) var(--gutter) var(--s3);
		padding-block: var(--s2);
	}
	.header-info {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--s1);
		min-width: 0;
	}
	.title-row {
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-width: 0;
	}
	.header-title {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.mark {
		width: var(--mark-large);
		height: var(--mark-large);
		border-radius: var(--radius-round);
		background: var(--muted);
		flex-shrink: 0;
	}
	.mark.running {
		background: var(--accent);
	}
	.subtitle {
		font-size: var(--fs-sm);
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.breadcrumb, .lane-intro, .lane-footer {
		box-sizing: border-box;
		width: calc(100% - 2 * var(--gutter));
		max-width: calc(var(--reading-max) - 2 * var(--gutter));
		margin-inline: auto;
	}
	.breadcrumb {
		display: flex;
		flex-direction: column;
		gap: var(--s1);
		margin-block: var(--s0) var(--s4);
		padding: var(--s2) var(--s6) var(--s4);
		border-radius: var(--radius-card);
		background: var(--surface);
		font-size: var(--fs-meta);
		color: var(--secondary);
	}
	.path {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--s3);
		margin: 0;
	}
	.breadcrumb a {
		display: inline-flex;
		align-items: center;
		min-height: var(--target);
		color: var(--secondary);
		text-decoration: underline;
		text-underline-offset: var(--s1);
	}
	.live-line {
		display: flex;
		align-items: center;
		gap: var(--s3);
		margin: 0;
		font-size: var(--fs-sm);
	}
	.live-dot {
		flex: none;
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--muted);
	}
	.live-dot.active {
		background: var(--accent);
	}
	.lane-intro {
		margin-block: var(--s0) var(--s3);
		padding: var(--s5) var(--s6);
		border-radius: var(--radius-card);
		background: var(--accent-soft);
	}
	.lane-intro strong {
		font-weight: var(--weight-semibold);
	}
	.lane-intro p {
		margin: var(--s2) 0 0;
		color: var(--secondary);
		font-size: var(--fs-meta);
		overflow-wrap: anywhere;
	}
	.lane-timeline-container {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
	.lane-footer {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: var(--s3);
		padding-block: var(--s3) max(var(--s4), env(safe-area-inset-bottom));
	}
	.pending-line {
		margin: 0;
		padding: var(--s3) var(--s6);
		border-radius: var(--radius-pill);
		background: var(--surface);
		color: var(--accent-link);
		font-size: var(--fs-sm);
	}
	.return-link {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-height: var(--target);
		padding-inline: var(--s8);
		border-radius: var(--radius-pill);
		background: var(--raised);
		color: var(--text);
		font-size: var(--fs-meta);
	}
	.topbar > .icon-button:first-child {
		margin-left: var(--icon-hang);
	}
	.topbar > .icon-button:last-child {
		margin-right: var(--icon-hang);
	}
	.icon-button {
		background: var(--transparent);
		border: none;
		color: var(--text);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		flex: none;
		width: var(--target);
		height: var(--target);
		border-radius: var(--radius-round);
	}
</style>
