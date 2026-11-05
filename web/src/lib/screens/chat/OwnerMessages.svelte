<script lang="ts">
	import { tick, untrack } from 'svelte';
	import { fmt } from '#lib/format/ru.js';
	import type { ChatStore, OwnerInputView } from '#lib/stores/chat.svelte.js';
	import { messagePreview, ownerMessageOrigin, sortOwnerMessages } from '#lib/timeline/ownerMessages.js';
	import Icon from '#lib/ui/Icon.svelte';

	let {
		chat,
		onpick,
		oncount,
		fill = false
	}: {
		chat: ChatStore;
		onpick: (item: OwnerInputView) => void;
		oncount?: (count: number) => void;
		fill?: boolean;
	} = $props();

	const PAGE = 500;
	const MAX = 5000;
	const now = Date.now();
	let items = $state<OwnerInputView[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let list = $state<HTMLDivElement | null>(null);
	let probe = $state<HTMLDivElement | null>(null);
	let viewport = $state(0);
	let scrollTop = $state(0);
	let rowHeight = $state(0);
	let request = 0;

	const height = $derived(rowHeight || 56);
	const range = $derived.by(() => {
		const rows = items.length;
		if (rows === 0) return { start: 0, end: 0, padTop: 0, padBottom: 0 };
		const view = viewport || 400;
		const start = Math.max(0, Math.floor(scrollTop / height) - 6);
		const end = Math.min(rows, Math.ceil((scrollTop + view) / height) + 6);
		return { start, end, padTop: start * height, padBottom: (rows - end) * height };
	});
	const visible = $derived(items.slice(range.start, range.end));

	$effect(() => {
		oncount?.(items.length);
	});

	$effect(() => {
		void chat.ownSends;
		untrack(() => void load());
	});

	$effect(() => {
		if (probe) rowHeight = probe.getBoundingClientRect().height || rowHeight;
	});

	$effect(() => {
		const el = list;
		if (!el) return;
		const observer = new ResizeObserver(() => {
			viewport = el.clientHeight;
		});
		observer.observe(el);
		viewport = el.clientHeight;
		return () => observer.disconnect();
	});
	function atBottom(): boolean {
		if (!list) return true;
		return list.scrollHeight - list.scrollTop - list.clientHeight < 24;
	}

	function bottom(): void {
		if (!list) return;
		list.scrollTop = list.scrollHeight;
		scrollTop = list.scrollTop;
	}

	function onscroll(): void {
		scrollTop = list?.scrollTop ?? 0;
	}

	async function load(): Promise<void> {
		const generation = ++request;
		const wasAtBottom = atBottom();
		if (items.length === 0) loading = true;
		error = null;
		const all: OwnerInputView[] = [];
		let before: string | undefined;
		try {
			for (let page = 0; page < 10; page++) {
				const next = await chat.ownerMessages({ before, limit: PAGE });
				if (generation !== request) return;
				all.push(...next.items);
				if (!next.next_before || all.length >= MAX) break;
				before = next.next_before;
			}
			items = sortOwnerMessages(all);
			loading = false;
			await tick();
			if (wasAtBottom) bottom();
		} catch {
			if (generation !== request) return;
			error = 'Не удалось загрузить сообщения';
			loading = false;
		}
	}
</script>

<div class="owner-messages" class:fill>
	<div class="probe" bind:this={probe} aria-hidden="true"></div>
	{#if loading && items.length === 0}
		<p class="empty">Загружаю сообщения…</p>
	{:else if error}
		<p class="empty" role="alert">{error}</p>
	{:else if items.length === 0}
		<p class="empty" data-owner-empty>Сообщений пока нет</p>
	{:else}
		<div class="list" bind:this={list} onscroll={onscroll} aria-label="Мои сообщения" data-owner-list>
			<div class="pad" style:height={`${range.padTop}px`}></div>
			{#each visible as item (item.id)}
				<button
					type="button"
					class="item"
					data-owner-message={item.id}
					data-owner-seq={item.seq}
					onclick={() => onpick(item)}
				>
					<span class="preview">{messagePreview(item.preview, item.attachments_count)}</span>
					<span class="meta">
						<span class="time">{fmt.messageTime(Date.parse(item.created_at), now)}</span>
						{#if ownerMessageOrigin(item.origin)}
							<span class="origin">{ownerMessageOrigin(item.origin)}</span>
						{/if}
						{#if item.attachments_count > 0}
							<span class="files">
								<span class="sr-only">Вложений: {item.attachments_count}</span>
								<Icon name="attach" />
							</span>
						{/if}
					</span>
				</button>
			{/each}
			<div class="pad" style:height={`${range.padBottom}px`}></div>
		</div>
	{/if}
</div>

<style>
	.owner-messages {
		position: relative;
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
	}
	.owner-messages.fill {
		height: 100%;
	}
	.probe {
		position: absolute;
		visibility: hidden;
		height: calc(var(--s14) + var(--s4));
		width: var(--s0);
	}
	.list {
		height: min(58dvh, calc(var(--sheet-max) - var(--s15)));
		overflow-y: auto;
		overscroll-behavior: contain;
		scrollbar-gutter: stable;
	}
	.fill .list {
		flex: 1;
		min-height: var(--s0);
		height: auto;
	}
	.pad {
		width: 100%;
	}
	.item {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: var(--s1);
		box-sizing: border-box;
		width: 100%;
		height: calc(var(--s14) + var(--s4));
		padding: var(--s2) var(--s4);
		border-radius: var(--radius-sm);
		background: var(--transparent);
		color: var(--text);
		text-align: start;
	}
	.item:focus-visible {
		outline: var(--focus-width) solid var(--accent);
		outline-offset: calc(var(--focus-width) * -1);
	}
	.preview {
		display: block;
		width: 100%;
		overflow: hidden;
		font-size: var(--fs-meta);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.meta {
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-width: 0;
		color: var(--secondary);
		font-size: var(--fs-sm);
	}
	.time {
		flex: none;
		font-variant-numeric: tabular-nums;
	}
	.origin {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.files {
		display: inline-flex;
		flex: none;
		color: var(--secondary);
	}
	.files :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	.empty {
		margin: var(--s0);
		padding: var(--s8) var(--s6);
		color: var(--muted);
		font-size: var(--fs-meta);
		text-align: center;
	}
</style>
