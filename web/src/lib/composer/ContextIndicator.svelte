<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import Button from '#lib/ui/Button.svelte';
	import Gauge from '#lib/ui/Gauge.svelte';
	import { turnIsLive } from '#lib/composer/sendRules.js';
	import { modelLabel } from '#lib/format/labels.js';
	import { executionLive, type ChatStore } from '#lib/stores/chat.svelte.js';
	import { compactSessionContext } from '#lib/screens/chat/controls.js';
	import {
		contextBadge,
		contextDetails,
		contextPercent,
		contextRemaining,
		contextTone,
		compactionRunningLine,
		type ContextTone
	} from '#lib/timeline/compaction.js';

	let { chat }: { chat: ChatStore } = $props();
	let open = $state(false);
	let armed = $state(false);
	let busy = $state(false);
	let error = $state('');
	let detailsNode = $state<HTMLElement | null>(null);

	const snapshot = $derived(chat.context);
	const usedPercent = $derived(snapshot ? contextPercent(snapshot.tokens, snapshot.window) : null);
	const remaining = $derived(snapshot ? contextRemaining(snapshot.tokens, snapshot.window) : null);
	const compacting = $derived(chat.compacting);
	// Сжатие и есть ответ на полный контекст: пока оно идёт, тревожный цвет не нужен.
	const tone: ContextTone = $derived(compacting ? 'accent' : contextTone(usedPercent));
	const gaugeTone = $derived(tone === 'accent' ? 'normal' : tone);
	const badge = $derived(snapshot ? contextBadge(snapshot.tokens, snapshot.window) : '');
	const label = $derived(compacting ? compactionRunningLine() : badge);
	// Подпись режется по ширине ячейки: «Контекст: ост. 50%» → «ост. 50%» → «50%». Полный текст остаётся в aria-label.
	const parts = $derived.by(() => {
		const match = /^(Контекст: )?(ост\. )?(.*)$/.exec(label);
		return { head: match?.[1] ?? '', mid: match?.[2] ?? '', tail: match?.[3] ?? label };
	});
	const details = $derived(snapshot ? contextDetails(snapshot.tokens, snapshot.window) : '');
	const model = $derived(
		modelLabel((chat.detail as { model?: string } | null)?.model ?? '')
	);
	const turnLive = $derived(turnIsLive(chat.activity, chat.queue.run_slot));
	const execLive = $derived(executionLive(chat.execution));
	const disabledReason = $derived(
		turnLive
			? 'Сжатие доступно между ходами'
			: !execLive
				? 'Нет живого исполнения для сжатия'
				: ''
	);

	$effect(() => {
		if (disabledReason) armed = false;
	});

	// Раскрытые подробности могут не поместиться в предел поля: долистываем капсулу до кнопки,
	// иначе «Сжать контекст» остаётся за нижней кромкой окна.
	$effect(() => {
		if (!open || !detailsNode) return;
		let host: HTMLElement | null = detailsNode.parentElement;
		while (host) {
			const style = getComputedStyle(host);
			if (/(auto|scroll)/.test(style.overflowY) && host.scrollHeight > host.clientHeight + 1) {
				host.scrollTop = host.scrollHeight;
				return;
			}
			host = host.parentElement;
		}
	});

	function toggle(): void {
		open = !open;
		armed = false;
		error = '';
	}

	async function squeeze(): Promise<void> {
		if (disabledReason || busy) return;
		if (!armed) {
			armed = true;
			return;
		}
		armed = false;
		busy = true;
		error = '';
		try {
			await compactSessionContext(chat.id);
			chat.noteCompactionRequested();
			open = false;
		} catch (err) {
			error = errorText(err, 'Сжать не удалось');
		} finally {
			busy = false;
		}
	}
</script>

{#if snapshot}
	<span class="cell">
	<button
		type="button"
		class="ctx"
		data-tone={tone}
		data-compacting={compacting ? 'true' : undefined}
		aria-expanded={open}
		aria-label={label.startsWith('Контекст') ? label : `Контекст: ${label}`}
		onclick={toggle}
	>
		{#if remaining !== null}
			<span class="track"><Gauge value={remaining} tone={gaugeTone} quiet label={badge} /></span>
		{/if}
		<span class="label">{#if parts.head}<span class="head">{parts.head}</span>{/if}{#if parts.mid}<span class="mid">{parts.mid}</span>{/if}{parts.tail}</span>
	</button>
	</span>
	{#if open}
		<div class="ctx-details" data-details bind:this={detailsNode}>
			<p class="tokens">{details}</p>
			{#if model}<p class="model">{model}</p>{/if}
			{#if disabledReason}
				<Button disabled={true} reason={disabledReason} onclick={squeeze}>Сжать контекст</Button>
			{:else}
				<Button onclick={squeeze}>{busy ? 'Сжимается…' : armed ? 'Точно сжать?' : 'Сжать контекст'}</Button>
			{/if}
			{#if error}<p class="caption" role="status">{error}</p>{/if}
		</div>
	{/if}
{/if}

<style>
	/* Ячейка отдаёт кнопке ровно ту ширину, что осталась в строке после кнопок: по ней подпись выбирает вид. */
	.cell {
		grid-area: context;
		container-type: inline-size;
		min-width: 0;
		font-size: var(--fs-sm);
	}
	.ctx {
		max-width: 100%;
		display: inline-flex;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
		min-height: var(--target);
		padding-inline: var(--s2);
		overflow: hidden;
		border: var(--s0);
		border-radius: var(--radius-pill);
		background: var(--transparent);
		color: var(--muted);
		font-size: var(--fs-sm);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.ctx[data-tone='warning'] {
		color: var(--warning);
	}
	.ctx[data-tone='danger'] {
		color: var(--danger);
	}
	/* В узкой строке уступает шкала, а не подпись: «Контекст: ост. 37%» и «Сжимает контекст» видны целиком.
	   Не хватило и на подпись: сначала уходит «Контекст:», потом «ост.», а на самой малой ширине остаётся
	   число с процентом. Последний предел — многоточие, наложения кнопок не бывает. */
	.track {
		display: none;
		flex: 1 1 0;
		max-width: var(--gauge-width);
		min-width: var(--s13);
	}
	.label {
		flex: 0 1 auto;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.head, .mid { display: none; }
	@container (min-width: 6.5em) { .mid { display: inline; } }
	@container (min-width: 10em) { .head { display: inline; } }
	@container (min-width: 14.5em) { .track { display: flex; } }
	/* Пока идёт сжатие, заполнение шкалы сжимается к началу и возвращается в такт строке ленты. */
	.ctx[data-compacting='true'] .track :global([role='meter'] > span) {
		transform-origin: left center;
		animation: ctx-squeeze var(--duration-compact-fold) var(--ease-compact-fold) infinite;
	}
	@keyframes ctx-squeeze {
		0%,
		25%,
		100% {
			transform: scaleX(1);
		}
		50%,
		65% {
			transform: scaleX(.55);
		}
	}
	.ctx-details {
		grid-area: details;
		display: flex;
		flex-direction: column;
		gap: var(--s2);
		padding: var(--s3) var(--s5);
		border-top: var(--s1) solid var(--line);
		font-size: var(--fs-sm);
	}
	.tokens,
	.model,
	.caption {
		margin: 0;
	}
	.tokens {
		color: var(--text);
		font-variant-numeric: tabular-nums;
	}
	.model,
	.caption {
		color: var(--muted);
	}
	:global(.capsule):has([data-details]) {
		grid-template-areas:
			'input input input input input input input'
			'attach mic context goal clock steer send'
			'details details details details details details details';
	}
	:global(.capsule):has(:global(.tray)):has([data-details]) {
		grid-template-areas:
			'tray tray tray tray tray tray tray'
			'input input input input input input input'
			'attach mic context goal clock steer send'
			'details details details details details details details';
	}
</style>
