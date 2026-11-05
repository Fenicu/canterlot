<script lang="ts">
	import type { CompactionModel } from '#lib/stores/timeline.js';
	import { compactionLine, compactionRunningLine } from '#lib/timeline/compaction.js';

	let { compaction }: { compaction: CompactionModel } = $props();
	const running = $derived(compaction.ok === null);
	const text = $derived(
		running
			? compactionRunningLine()
			: compactionLine({ trigger: compaction.trigger, ok: compaction.ok, pre_tokens: compaction.preTokens })
	);

	// Строка, которую владелец видел идущей, при итоге оседает: строки сходятся, подпись встаёт на место.
	// Перечитанная готовая строка появляется сразу, без движения.
	let watched = false;
	let settling = $state(false);
	$effect.pre(() => {
		if (running) {
			watched = true;
			settling = false;
			return;
		}
		if (!watched) return;
		watched = false;
		settling = !matchMedia('(prefers-reduced-motion: reduce)').matches;
	});
</script>

<p
	class="compact"
	data-running={running ? 'true' : undefined}
	data-settling={settling ? 'true' : undefined}
	data-failed={compaction.ok === false ? 'true' : undefined}
>
	{#if running || settling}
		<span
			class="fold"
			aria-hidden="true"
			onanimationend={(event) => {
				if (event.animationName.endsWith('compact-settle-fold')) settling = false;
			}}
		><i></i><i></i><i></i></span>
	{/if}
	<span class="text">{text}</span>
</p>

<style>
	.compact {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--s3);
		margin: 0;
		padding-inline: var(--s6);
		color: var(--muted);
		font-size: var(--fs-sm);
	}
	.compact[data-failed='true'] {
		color: var(--danger);
	}
	.text {
		min-width: 0;
	}

	/* Три строки текста: длины разные, как у абзаца, правый край рваный. */
	.fold {
		position: relative;
		flex: none;
		width: var(--compact-fold-width);
		height: var(--compact-fold-height);
		color: var(--accent);
	}
	.fold i {
		position: absolute;
		left: 0;
		width: 100%;
		height: var(--compact-fold-line);
		border-radius: var(--radius-pill);
		background: currentColor;
		transform-origin: left center;
	}
	.fold i:nth-child(1) {
		top: 0;
		transform: scaleX(1);
		animation: compact-fold-top var(--duration-compact-fold) var(--ease-compact-fold) infinite;
	}
	.fold i:nth-child(2) {
		top: calc(50% - var(--compact-fold-line) / 2);
		transform: scaleX(.6);
		opacity: .7;
		animation: compact-fold-middle var(--duration-compact-fold) var(--ease-compact-fold) infinite;
	}
	.fold i:nth-child(3) {
		bottom: 0;
		transform: scaleX(.8);
		opacity: .85;
		animation: compact-fold-bottom var(--duration-compact-fold) var(--ease-compact-fold) infinite;
	}

	/* Строки сходятся к средней и ложатся одним плотным бруском, потом расходятся снова. */
	@keyframes compact-fold-top {
		0%,
		22%,
		100% {
			transform: translateY(0) scaleX(1);
		}
		48%,
		66% {
			transform: translateY(var(--s2)) scaleX(.7);
		}
	}
	@keyframes compact-fold-middle {
		0%,
		24%,
		100% {
			transform: scaleX(.6);
			opacity: .7;
		}
		48%,
		66% {
			transform: scaleX(.7) scaleY(2);
			opacity: 1;
		}
	}
	@keyframes compact-fold-bottom {
		0%,
		26%,
		100% {
			transform: translateY(0) scaleX(.8);
		}
		50%,
		66% {
			transform: translateY(calc(var(--s2) * -1)) scaleX(.7);
		}
	}

	/* Итог: строки сходятся в последний раз, брусок уходит в подпись, подпись встаёт на отступ текста. */
	.compact[data-settling='true'] .fold {
		position: absolute;
		left: var(--s6);
		top: calc(50% - var(--compact-fold-height) / 2);
	}
	.compact[data-settling='true'] .fold i:nth-child(1) {
		animation: compact-settle-top var(--duration-compact-settle) var(--ease-enter) forwards;
	}
	.compact[data-settling='true'] .fold i:nth-child(2) {
		animation: compact-settle-fold var(--duration-compact-settle) var(--ease-enter) forwards;
	}
	.compact[data-settling='true'] .fold i:nth-child(3) {
		animation: compact-settle-bottom var(--duration-compact-settle) var(--ease-enter) forwards;
	}
	.compact[data-settling='true'] .text {
		animation: compact-settle-text var(--duration-compact-settle) var(--ease-enter) both;
	}
	@keyframes compact-settle-top {
		45% {
			transform: translateY(var(--s2)) scaleX(.7);
			opacity: 1;
		}
		100% {
			transform: translateY(var(--s2)) scaleX(0);
			opacity: 0;
		}
	}
	@keyframes compact-settle-fold {
		45% {
			transform: scaleX(.7) scaleY(2);
			opacity: 1;
		}
		100% {
			transform: scaleX(0) scaleY(2);
			opacity: 0;
		}
	}
	@keyframes compact-settle-bottom {
		45% {
			transform: translateY(calc(var(--s2) * -1)) scaleX(.7);
			opacity: 1;
		}
		100% {
			transform: translateY(calc(var(--s2) * -1)) scaleX(0);
			opacity: 0;
		}
	}
	@keyframes compact-settle-text {
		0%,
		35% {
			transform: translateX(calc(var(--compact-fold-width) + var(--s3)));
			opacity: .4;
		}
		100% {
			transform: translateX(0);
			opacity: 1;
		}
	}
	.compact[data-failed='true'][data-settling='true'] .fold {
		color: var(--danger);
	}

</style>
