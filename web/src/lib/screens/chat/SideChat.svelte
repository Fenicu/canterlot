<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { createSideChat, deleteSideChat, postInput } from '#lib/api/client.js';
	import { ticker } from '#lib/ticker.svelte.js';
	import Composer from '#lib/composer/Composer.svelte';
	import RequestCard from '#lib/requests/RequestCard.svelte';
	import { isDocked } from '#lib/stores/requests.svelte.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Timeline from '#lib/timeline/Timeline.svelte';
	import StateChip from './StateChip.svelte';
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import IconButton from '#lib/ui/IconButton.svelte';
	import Sheet from '#lib/ui/Sheet.svelte';
	import { onDestroy, onMount } from 'svelte';
	import { leaseChatSession, type ChatLease } from './open.js';

	let {
		parent,
		question = '',
		serial = 0,
		now = Date.now(),
		onclose
	}: {
		parent: ChatStore;
		/** Вопрос из `/side вопрос`: уходит сразу после открытия копии. */
		question?: string;
		/** Номер вопроса: повторный `/side` с тем же текстом тоже отправляется. */
		serial?: number;
		now?: number;
		onclose: () => void;
	} = $props();

	void ticker.second;
	const clock = $derived.by(() => {
		void ticker.second;
		return now;
	});

	let chat = $state<ChatStore | null>(null);
	let lease: ChatLease | null = null;
	let error = $state<string | null>(null);
	let opening = $state(false);
	let closing = $state(false);

	async function send(text: string) {
		const id = chat?.id;
		if (!id || !text.trim()) return;
		try {
			await postInput(
				id,
				{ content: [{ type: 'text', text }] },
				crypto.randomUUID()
			);
		} catch (err: unknown) {
			error = errorText(err, 'Не удалось отправить вопрос');
		}
	}

	async function open() {
		if (opening) return;
		opening = true;
		error = null;
		try {
			const response = await createSideChat(parent.id);
			const id = response.data.conversation_id;
			lease = leaseChatSession(id);
			chat = await lease.chat;
		} catch (err: unknown) {
			error = errorText(err, 'Не удалось открыть боковой чат');
		} finally {
			opening = false;
		}
	}

	async function restart() {
		if (closing) return;
		closing = true;
		error = null;
		try {
			await deleteSideChat(parent.id);
			lease?.release();
			lease = null;
			chat = null;
			await open();
		} catch (err: unknown) {
			error = errorText(err, 'Не удалось начать заново');
		} finally {
			closing = false;
		}
	}

	// Открытие и закрытие листа не останавливают копию: отпускается только подписка ленты.
	onMount(() => {
		void open();
		return () => {
			lease?.release();
			lease = null;
		};
	});

	onDestroy(() => lease?.release());

	let sentSerial = -1;
	$effect(() => {
		if (!chat || !question.trim() || serial === sentSerial) return;
		sentSerial = serial;
		void send(question);
	});

	const title = $derived((parent.detail as { title?: string } | null)?.title ?? 'Сессия');
	const docked = $derived(
		chat ? [...chat.requests.items.values()].filter(isDocked) : []
	);
	const harnessId = $derived(
		((chat?.detail as { harness?: { id?: string } } | null)?.harness?.id) ??
			((parent.detail as { harness?: { id?: string } } | null)?.harness?.id) ??
			''
	);
	const permissionMode = $derived(
		((chat?.detail as { permission_mode?: string } | null)?.permission_mode) ?? 'read_only'
	);
	const freshAt = $derived(
		(chat?.detail as { side_fresh_at?: string | null } | null)?.side_fresh_at ?? null
	);
	const parentState = $derived(
		(parent.execution as { state?: string } | null)?.state ?? null
	);


</script>

{#snippet restartAction()}
	<IconButton icon="retry" label="Начать заново" inert={closing} onclick={() => void restart()} />
{/snippet}

{#snippet body()}
	<div class="side-body">
		<div class="side-notes">
			<StateChip activity={parent.activity as Activity | null} phase={parent.phase} executionState={parentState} now={clock} />
			{#if error}<p class="error" role="alert">{error}</p>{/if}
			{#if !chat}
				<p class="opening">Открываю копию сессии…</p>
			{:else if freshAt}
				<p class="service">Начато заново от свежей копии сессии</p>
			{/if}
		</div>
		{#if chat}
			<Timeline chat={chat} {now} markable={false} />
			{#each docked as req (req.id)}
				<RequestCard
					request={req}
					arrive
					{now}
					harnessId={harnessId}
					permissionMode={permissionMode}
					onDecision={async (decision) => {
						const store = chat;
						if (store) await store.requests.submitDecision(req.id, decision, store.ops);
					}}
				/>
			{/each}
			<Composer {chat} hint="Вопрос в стороне…" />
		{/if}
	</div>
{/snippet}

{#if pane.desktop}
	<aside class="side-panel" aria-labelledby="side-panel-title">
		<header>
			<h2 id="side-panel-title"><span class="name">{title}</span> <span class="marker">· в стороне</span></h2>
			<div class="actions">
				{@render restartAction()}
				<IconButton icon="close" label="Закрыть боковой чат" onclick={onclose} />
			</div>
		</header>
		{@render body()}
	</aside>
{:else}
	<Sheet open {title} marker="в стороне" feed {onclose} actions={restartAction}>
		{@render body()}
	</Sheet>
{/if}

<style>
	/* Лента и поле стоят как в основном чате: прокручивается сама лента, поле прижато к ней снизу. */
	.side-body {
		flex: 1 1 auto;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.side-body > :global(.feed-wrap) {
		flex: 0 1 auto;
		min-height: var(--s15);
	}
	/* Панель на всю высоту окна: поле внизу, как у основной ленты. */
	.side-panel .side-body > :global(.feed-wrap) {
		flex: 1 1 auto;
	}
	/* В основном чате между лентой и полем стоит полоса очереди с отступами: копия держит тот же зазор. */
	.side-body > :global(.composer) {
		margin-top: var(--s6);
	}
	.side-notes {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		flex: none;
		padding-inline: var(--gutter);
	}
	.side-notes:empty {
		display: none;
	}
	.side-panel {
		position: fixed;
		inset: 0 0 0 auto;
		width: var(--sidebar-width);
		display: flex;
		flex-direction: column;
		background: var(--chat-bg);
		border-left: var(--s1) solid var(--line);
		z-index: 30;
	}
	.side-panel header {
		flex: none;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--s4);
		padding: var(--s4) var(--s4) var(--s2) var(--s8);
	}
	.side-panel h2 {
		display: flex;
		align-items: baseline;
		gap: var(--s1);
		min-width: 0;
		margin: 0;
		font-size: var(--fs-title);
		white-space: nowrap;
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.marker {
		flex: none;
		color: var(--muted);
		font-size: var(--fs-meta);
		font-weight: var(--weight-normal);
	}
	.actions {
		display: flex;
		align-items: center;
		gap: var(--s2);
		flex: none;
	}
	.opening,
	.service {
		margin: 0;
		color: var(--muted);
		font-size: var(--fs-sm);
	}
	.error {
		margin: 0;
		color: var(--danger);
	}
</style>
