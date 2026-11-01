<script lang="ts">
	import type { Activity } from '#lib/screens/sessions/activityText.js';
	import { announce } from '#lib/a11y/announce.js';
	import { chatStateLine } from './stateLine';

	let {
		activity = null,
		phase = null,
		executionState = null,
		now = Date.now()
	}: {
		activity?: Activity | null;
		phase?: { phase?: string } | null;
		executionState?: string | null;
		now?: number;
	} = $props();

	const line = $derived(chatStateLine(activity, phase, executionState, now));

	let prevText: string | null = null;
	$effect(() => {
		const currentText = line.text;
		if (prevText !== null && prevText !== currentText) {
			announce(currentText, 'polite');
		}
		prevText = currentText;
	});
</script>

<div class="state-chip tone-{line.tone}" class:live={line.live} class:sr-only={line.quiet}>
	{#if line.live}
		<span class="live-dot" aria-hidden="true"></span>
	{/if}
	<span class="state-text">{line.text}</span>
</div>

<style>
	.state-chip {
		display: inline-flex;
		align-items: center;
		flex: 0 1 auto;
		gap: var(--s2);
		min-width: 0;
		font-size: var(--fs-sm);
		line-height: var(--lh-tight);
		color: var(--muted);
	}
	.state-chip.tone-accent {
		color: var(--accent-link);
	}
	.state-chip.tone-warning {
		color: var(--warning);
	}
	.state-chip.tone-danger {
		color: var(--danger);
	}
	.state-text {
		font-variant-numeric: tabular-nums;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.live-dot {
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--accent);
		display: inline-block;
		flex-shrink: 0;
	}
</style>
