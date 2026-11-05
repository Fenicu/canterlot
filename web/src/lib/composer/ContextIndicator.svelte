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
		contextTone,
		compactionRunningLine,
		type ContextTone
	} from '#lib/timeline/compaction.js';

	let { chat }: { chat: ChatStore } = $props();
	let open = $state(false);
	let armed = $state(false);
	let busy = $state(false);
	let error = $state('');

	const snapshot = $derived(chat.context);
	const percent = $derived(snapshot ? contextPercent(snapshot.tokens, snapshot.window) : null);
	const compacting = $derived(chat.compacting);
	// Сжатие и есть ответ на полный контекст: пока оно идёт, тревожный цвет не нужен.
	const tone: ContextTone = $derived(compacting ? 'accent' : contextTone(percent));
	const gaugeTone = $derived(tone === 'accent' ? 'normal' : tone);
	const badge = $derived(snapshot ? contextBadge(snapshot.tokens, snapshot.window) : '');
	const label = $derived(compacting ? compactionRunningLine() : badge);
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
	<button
		type="button"
		class="ctx"
		data-tone={tone}
		data-compacting={compacting ? 'true' : undefined}
		aria-expanded={open}
		aria-label="Контекст: {label}"
		onclick={toggle}
	>
		<span class="track"><Gauge value={percent ?? 0} tone={gaugeTone} dim={percent === null} label={badge} /></span>
		<span class="label">{label}</span>
	</button>
	{#if open}
		<div class="ctx-details" data-details>
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
	.ctx {
		grid-area: context;
		display: inline-flex;
		align-items: center;
		gap: var(--s2);
		min-width: 0;
		min-height: var(--target);
		padding-inline: var(--s2);
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
	.track {
		width: var(--gauge-width);
		max-width: var(--gauge-width);
		min-width: 0;
	}
	.label {
		overflow: hidden;
		text-overflow: ellipsis;
	}
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
		grid-template-areas: 'input input input input input' 'attach context extras clock send' 'details details details details details';
	}
	:global(.capsule):has(:global(.tray)):has([data-details]) {
		grid-template-areas: 'tray tray tray tray tray' 'input input input input input' 'attach context extras clock send' 'details details details details details';
	}
</style>
