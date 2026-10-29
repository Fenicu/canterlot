<script lang="ts">
	import { resolve } from '$app/paths';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Timeline from '#lib/timeline/Timeline.svelte';
	import { laneModel } from './lane';
	import { theme } from '#lib/theme.js';

	let {
		chat,
		rootBlockId,
		now = Date.now(),
		onInfo
	}: {
		chat: ChatStore;
		rootBlockId: string;
		now?: number;
		onInfo?: (runId: string) => void;
	} = $props();

	const blocks = $derived([...chat.blocks.values()]);
	const machineName = $derived((chat.detail?.machine as { name?: string })?.name || 'workstation');

	const model = $derived(
		laneModel(blocks, rootBlockId, { machineName })
	);
</script>

<div class="lane-screen" data-ready>
	<header class="topbar lane-header">
		<a
			class="icon-button back"
			href={resolve(`c/${chat.id}`)}
			aria-label="К оркестратору"
		><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m13 5-7 7 7 7"></path></svg></a>

		<div class="grow header-info">
			<div class="row title-row">
				<span class="mark"></span>
				<h1 class="header-title">{model.title}</h1>
			</div>
			<div class="subtitle">{model.subtitle}</div>
		</div>
		<button
			type="button"
			class="icon-button symbol"
			data-theme-toggle
			aria-label="Сменить тему"
			onclick={() => theme.toggle()}
		>
			◐
		</button>
	</header>

	<nav class="breadcrumb" aria-label="Путь делегирования">
		<a href={resolve(`c/${chat.id}`)}>{model.breadcrumb[0]}</a>
		→ 
		<span class="secondary">{model.breadcrumb[1]}</span>

		<div class="live-line">
			<span class="live-dot" class:active={model.running}></span>
			{model.status}
		</div>
	</nav>

	{#if model.task}
		<div class="lane-intro">
			<div class="row between">
				<strong>Задача от оркестратора</strong>
			</div>
			<p>{model.task}</p>
		</div>
	{/if}

	<div class="lane-timeline-container">
		<Timeline
			chat={chat}
			onInfo={onInfo}
			now={now}
			laneRoot={rootBlockId}
		/>
	</div>

	<footer class="lane-footer">
		<aside class="pending">
			<div class="pending-line">
				<span class="secondary">Результат вернётся в основную сессию</span>
			</div>
		</aside>
		<div class="lane-nav-back">
			<a
				class="button return-link"
				href={resolve(`c/${chat.id}`)}
			>К основной сессии</a>
		</div>
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
		background: var(--surface);
		color: var(--text);
	}
	.lane-header {
		display: flex;
		align-items: center;
		gap: var(--s2);
		padding: var(--s2) var(--s4);
		border-bottom: var(--hairline) solid var(--line);
		min-height: var(--target);
	}
	.header-info {
		min-width: 0;
	}
	.title-row {
		display: flex;
		align-items: center;
		gap: var(--s2);
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
		background: var(--accent);
		flex-shrink: 0;
	}
	.subtitle {
		font-size: var(--fs-meta);
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.breadcrumb {
		display: flex;
		align-items: center;
		gap: var(--s2);
		padding: var(--s2) var(--s4);
		background: var(--raised);
		font-size: var(--fs-meta);
		color: var(--muted);
		border-bottom: var(--hairline) solid var(--line);
		flex-wrap: wrap;
	}
	.breadcrumb a {
		color: var(--text);
		text-decoration: none;
	}
	.breadcrumb a:hover {
		text-decoration: underline;
	}
	.secondary {
		color: var(--accent);
		font-weight: var(--weight-semibold);
	}
	.live-line {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: var(--s2);
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	.live-dot {
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--muted);
	}
	.live-dot.active {
		background: var(--accent);
	}
	.lane-intro {
		margin: var(--s3) var(--s4) 0;
		padding: var(--s3);
		border-radius: var(--radius-card);
		background: var(--raised);
		border: var(--hairline) solid var(--line);
		font-size: var(--fs-body);
	}
	.lane-intro p {
		margin: var(--s1) 0 0;
		color: var(--text);
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
		border-top: var(--hairline) solid var(--line);
		background: var(--surface);
		padding: var(--s2) var(--s4);
		gap: var(--s2);
	}
	.pending-line {
		display: flex;
		align-items: center;
		justify-content: center;
		padding: var(--s2);
		font-size: var(--fs-meta);
	}
	.lane-nav-back {
		display: flex;
		justify-content: center;
	}
	.return-link {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: var(--s2) var(--s4);
		border-radius: var(--radius-card);
		background: var(--raised);
		border: var(--hairline) solid var(--line);
		color: var(--accent-link);
		text-decoration: none;
		min-height: var(--target);
	}
	.icon-button {
		background: var(--transparent);
		border: none;
		color: var(--text);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		width: var(--target);
		height: var(--target);
		border-radius: var(--radius-card);
	}
	.icon-button svg {
		width: var(--icon);
		height: var(--icon);
		fill: none;
		stroke: currentColor;
		stroke-width: 2;
	}
	.grow {
		flex: 1;
	}
	.row {
		display: flex;
		align-items: center;
		gap: var(--s2);
	}
	.row.between {
		justify-content: space-between;
	}
</style>
