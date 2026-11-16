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
	const restartState = $derived(closing ? { 'aria-disabled': true } : {});

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

	const title = $derived(
		((parent.detail as { title?: string } | null)?.title ?? 'Сессия') + ' · в стороне'
	);
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
		<button type="button" class="restart" {...restartState} onclick={() => { if (!closing) void restart(); }}>
			{closing ? 'Начинаю…' : 'Начать заново'}
		</button>
	{/snippet}

{#if pane.desktop}
	<aside class="side-panel" aria-label="Боковой чат">
		<header>
			<div class="titles">
				<h2>{title}</h2>
				<StateChip activity={parent.activity as Activity | null} phase={parent.phase} executionState={parentState} now={clock} />
			</div>
			<div class="actions">
				{@render restartAction()}
				<IconButton icon="close" label="Закрыть боковой чат" onclick={onclose} />
			</div>
		</header>
		<div class="side-body">
			<StateChip activity={parent.activity as Activity | null} phase={parent.phase} executionState={parentState} now={clock} />
			{#if error}<p class="error" role="alert">{error}</p>{/if}
			{#if !chat}
				<p class="opening">Открываю копию сессии…</p>
			{:else}
				{#if freshAt}
					<p class="service">Начато заново от свежей копии сессии</p>
				{/if}
				<Timeline chat={chat} {now} />
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
				<Composer {chat} />
			{/if}
		</div>
	</aside>
{:else}
	<Sheet open {title} {onclose} actions={restartAction}>
		<div class="side-body">
			<StateChip activity={parent.activity as Activity | null} phase={parent.phase} executionState={parentState} now={clock} />
			{#if error}<p class="error" role="alert">{error}</p>{/if}
			{#if !chat}
				<p class="opening">Открываю копию сессии…</p>
			{:else}
				{#if freshAt}
					<p class="service">Начато заново от свежей копии сессии</p>
				{/if}
				<Timeline chat={chat} {now} />
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
				<Composer {chat} />
			{/if}
		</div>
	</Sheet>
{/if}

<style>
	.side-body {
		display: flex;
		flex-direction: column;
		gap: var(--s4);
		min-height: 0;
	}
	.side-body :global(.timeline) {
		min-height: 0;
	}
	.side-panel {
		position: fixed;
		inset: 0 0 0 auto;
		width: var(--sidebar-width);
		display: flex;
		flex-direction: column;
		background: var(--surface);
		border-left: var(--s1) solid var(--line);
		z-index: 30;
	}
	.side-panel header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--s4);
		padding: var(--s6) var(--s8) var(--s4);
	}
	.titles {
		min-width: 0;
	}
	.titles h2 {
		margin: 0;
		font-size: var(--fs-title);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.actions {
		display: flex;
		align-items: center;
		gap: var(--s4);
		flex: none;
	}
	.side-panel .side-body {
		padding: 0 var(--s8) var(--s8);
		overflow-x: hidden;
		overflow-y: auto;
	}
	.restart {
		min-height: var(--target);
		padding: 0 var(--s4);
		border: var(--s1) solid var(--line);
		white-space: nowrap;
		border-radius: var(--radius-pill);
		background: var(--surface);
		color: var(--text);
		font-size: var(--fs-sm);
	}
	.restart[aria-disabled="true"] {
		color: var(--muted);
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
