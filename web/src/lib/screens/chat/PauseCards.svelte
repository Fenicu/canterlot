<script lang="ts">
	import { executionLive, type ChatStore } from '#lib/stores/chat.svelte.js';
	import { RESOLVE_WARNING, pauseHoldCards, type CardAction, type QueueView } from './pauseCards';
	import {
		compactSessionContext,
		continueSessionQueue,
		resolveOwnerLost,
		resumeSession,
		spendSessionReserve,
		updateCrunch
	} from './controls';
	import { announce } from '#lib/a11y/announce.js';
	import { errorText } from '#lib/api/errors.js';
	import { retryAsNew } from '#lib/timeline/QueueActions.js';
	import Button from '#lib/ui/Button.svelte';
	import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';

	let {
		chat,
		now = Date.now()
	}: {
		chat: ChatStore;
		now?: number;
	} = $props();

	const provider = $derived((chat.detail?.harness as { provider?: string })?.provider ?? 'claude');
	const liveExecution = $derived(executionLive(chat.execution));

	const failure = $derived(chat.activity as { state?: string; evidence?: { reason?: string } } | null);

	const cards = $derived(
		pauseHoldCards(chat.queue as unknown as QueueView, {
			provider,
			crunch: chat.crunch.enabled,
			resumePending: chat.crunch.resume_pending,
			resumeText: chat.crunch.resume_text ?? null,
			liveExecution,
			now,
			turnRecorded: chat.turnRecorded,
			executionOwnerLost: chat.execution?.state === 'owner_lost',
			failureReason: failure?.state === 'failed' ? (failure.evidence?.reason ?? null) : null,
			inputs: chat.inputs.values()
		})
	);

	let spendMessage = $state<string | null>(null);
	let resumeText = $state('Лимиты восстановлены, продолжай с того места, где остановился.');
	let editingCrunch = $state(false);
	let savingCrunch = $state(false);
	let actionError = $state<string | null>(null);
	let acting = $state(false);
	let ask = $state<string | null>(null);
	let askResolve = $state(false);
	let decided = $state<((ok: boolean) => void) | null>(null);
	/** Развёрнутые списки карточек: свёрнутая карточка показывает первые три ввода. */
	let expandedCards = $state<Record<string, boolean>>({});

	function toggleEntries(key: string) {
		expandedCards[key] = !expandedCards[key];
	}

	function confirm(message: string): Promise<boolean> {
		ask = message;
		return new Promise((resolve) => {
			decided = resolve;
		});
	}

	function answer(ok: boolean) {
		decided?.(ok);
		decided = null;
		ask = null;
	}

	$effect(() => {
		if (chat.crunch.resume_text) {
			resumeText = chat.crunch.resume_text;
		}
	});

	// Announce changes in pause cards
	let prevKeys = '';
	$effect(() => {
		const currentKeys = cards.map((c) => c.title).join('; ');
		if (prevKeys && currentKeys !== prevKeys) {
			announce(currentKeys, 'assertive');
		}
		prevKeys = currentKeys;
	});

	async function handleAction(action: CardAction) {
		if (acting) return;
		spendMessage = null;
		actionError = null;
		acting = true;
		try {
			switch (action.id) {
				case 'continue_queue':
					await continueSessionQueue(chat.id);
					break;
				case 'compact_context':
					await compactSessionContext(chat.id);
					break;
				case 'resolve_owner_lost': {
					askResolve = true;
					const ok = await confirm(RESOLVE_WARNING);
					askResolve = false;
					if (ok) await resolveOwnerLost(chat.id);
					break;
				}
				case 'run':
					await resumeSession(chat.id, { continue_queue: chat.queue.pause_reasons.length > 0 });
					break;
				case 'spend_reserve': {
					const res = await spendSessionReserve(chat.id);
					if (res.message) spendMessage = res.message;
					break;
				}
				case 'edit_resume_text':
					editingCrunch = !editingCrunch;
					break;
				case 'retry_as_new': {
					const input = [...chat.inputs.values()].find((item) => item.id === action.inputId);
					if (input) await retryAsNew(input, { ops: chat.ops, chat, conversationId: chat.id, confirm });
					break;
				}
			}
		} catch (err) {
			actionError = errorText(err, 'Не удалось выполнить действие');
		} finally {
			acting = false;
		}
	}

	async function saveResumeText() {
		if (savingCrunch) return;
		savingCrunch = true;
		actionError = null;
		try {
			await updateCrunch(chat.id, {
				enabled: chat.crunch.enabled,
				resume_text: resumeText
			});
			editingCrunch = false;
		} catch (err) {
			actionError = errorText(err, 'Не удалось сохранить текст продолжения');
		} finally {
			savingCrunch = false;
		}
	}
</script>

{#if ask}
	<ConfirmDialog
		title={askResolve ? 'Завершить исполнение?' : 'Повторная отправка'}
		body={ask}
		confirmLabel={askResolve ? 'Завершить' : 'Отправить'}
		danger={askResolve}
		onconfirm={() => answer(true)}
		onclose={() => answer(false)}
	/>
{/if}

{#if cards.length > 0}
	<div class="pause-cards-feed" role="region" aria-label="Уведомления о состоянии очереди">
		{#each cards as card (card.key)}
			<section
				class="pause-card tone-{card.tone}"
				class:notice={card.tone === 'default'}
				data-tone={card.tone}
				aria-label={card.title}
			>
				<h2>{card.title}</h2>

				{#each card.body as paragraph, idx (idx)}
					<p>{paragraph}</p>
				{/each}

				{#if card.entries && card.entries.length > 0}
					{@const shown = expandedCards[card.key] ? card.entries : card.entries.slice(0, 3)}
					<ul class="entry-list">
						{#each shown as entry, idx (entry.key)}
							<li class="entry">
								{#if entry.from}
									<p class="entry-origin">{entry.from}</p>
								{/if}
								<div class="entry-row">
									<p class="entry-text" class:entry-unavailable={entry.unavailable}>
										{entry.excerpt}
									</p>
									<span class="entry-action">
										<Button
											variant="text"
											label="{entry.action.label}: сообщение {idx + 1}"
											onclick={() => handleAction(entry.action)}
										>
											Повторить
										</Button>
									</span>
								</div>
							</li>
						{/each}
					</ul>
					{#if card.entries.length > 3}
						<button
							type="button"
							class="button secondary full"
							aria-expanded={Boolean(expandedCards[card.key])}
							onclick={() => toggleEntries(card.key)}
						>
							{expandedCards[card.key] ? 'Свернуть' : `Показать все (${card.entries.length})`}
						</button>
					{/if}
				{/if}

				{#if spendMessage && card.actions.some((a) => a.id === 'spend_reserve')}
					<p class="card-note" role="status">{spendMessage}</p>
				{/if}

				{#if actionError && (card.actions.length > 0 || (card.entries?.length ?? 0) > 0)}
					<p class="card-error" role="alert">{actionError}</p>
				{/if}

				{#if card.actions.length > 0}
					<div class="card-actions">
						{#each card.actions as action (action.id)}
							{#if action.id === 'edit_resume_text'}
								<button
									type="button"
									class="button secondary full"
									onclick={() => handleAction(action)}
								>
									{action.label}
								</button>
								{#if editingCrunch}
									<div class="field edit-crunch-field">
										<label class="field-label" for="resume-paused-{card.key}">Сообщение после паузы</label>
										<textarea
											id="resume-paused-{card.key}"
											rows="3"
											bind:value={resumeText}
											disabled={savingCrunch}
										></textarea>
										<button
											type="button"
											class="button primary"
											onclick={saveResumeText}
										>
											{savingCrunch ? 'Сохранение…' : 'Сохранить'}
										</button>
									</div>
								{/if}
							{:else}
								<button
									type="button"
									class="button primary full"
									onclick={() => handleAction(action)}
								>
									{action.label}
								</button>
							{/if}
						{/each}
					</div>
				{/if}
			</section>
		{/each}
	</div>
{/if}

<style>
	.pause-cards-feed {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		padding: var(--s3) var(--gutter);
		box-sizing: border-box;
		width: 100%;
	}
	/* Поля как у карточек ленты: край и текст карточки совпадают с лентой над ней. */
	.pause-card {
		padding: var(--s5) var(--s6);
		border-radius: var(--radius-card);
		background: var(--surface);
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		color: var(--text);
	}
	.pause-card.tone-warning {
		background: var(--warning-soft);
	}
	.pause-card.tone-danger {
		background: var(--danger-soft);
	}
	.pause-card h2 {
		margin: 0;
		font-size: var(--fs-title);
		font-weight: var(--weight-semibold);
	}
	.pause-card p {
		margin: 0;
		font-size: var(--fs-body);
		color: var(--muted);
	}
	.entry-list {
		display: flex;
		flex-direction: column;
		gap: var(--s4);
		margin: var(--s2) 0 0;
		padding: 0;
		list-style: none;
	}
	.entry {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		min-width: 0;
		padding-top: var(--s4);
		border-top: var(--hairline) solid var(--line);
	}
	.entry:first-child {
		padding-top: var(--s0);
		border-top: var(--s0);
	}
	.pause-card .entry-origin {
		color: var(--secondary);
		font-size: var(--fs-sm);
		font-weight: var(--weight-medium);
	}
	/* Действие записи стоит на строке с текстом: стопка полноширинных кнопок не нужна. */
	.entry-row {
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-width: 0;
	}
	.entry-action {
		flex: none;
		display: flex;
		align-items: center;
	}
	.pause-card .entry-text {
		flex: 1 1 auto;
		min-width: 0;
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		color: var(--text);
		overflow-wrap: anywhere;
	}
	.pause-card .entry-text.entry-unavailable {
		color: var(--muted);
		font-style: italic;
	}
	.card-note {
		color: var(--accent);
		font-weight: var(--weight-medium);
	}
	.card-error {
		color: var(--danger);
	}
	.card-actions {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		margin-top: var(--s2);
	}
	.button {
		padding: var(--s3);
		border-radius: var(--radius-card);
		font-size: var(--fs-body);
		font-weight: var(--weight-semibold);
		cursor: pointer;
		text-align: center;
		border: none;
		min-height: var(--target);
	}
	.button.full {
		width: 100%;
	}
	.button.primary {
		background: var(--accent);
		color: var(--accent-ink);
	}
	.button.secondary {
		background: var(--raised);
		color: var(--text);
	}
	.edit-crunch-field {
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		margin-top: var(--s2);
	}
	.field-label {
		font-size: var(--fs-meta);
		color: var(--muted);
	}
	textarea {
		padding: var(--s2);
		border-radius: var(--radius-card);
		border: var(--s0);
		background: var(--raised);
		color: var(--text);
		font-size: var(--fs-body);
	}
</style>
