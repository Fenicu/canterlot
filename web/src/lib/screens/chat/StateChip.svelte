<script lang="ts">
	import { activityLine, type Activity } from '#lib/screens/sessions/activityText.js';
	import { announce } from '#lib/a11y/announce.js';

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

	const effectiveActivity = $derived.by(() => {
		if (executionState === 'stopping') {
			return { state: 'working' as const, evidence: { phase: 'stopping' } };
		}
		if (!activity) return { state: 'idle' as const };
		const p = phase?.phase;
		if (p && (!activity.evidence || !activity.evidence.phase)) {
			return {
				...activity,
				evidence: { ...(activity.evidence ?? {}), phase: p }
			};
		}
		return activity;
	});

	const line = $derived(activityLine(effectiveActivity, { now }));

	let prevText: string | null = null;
	$effect(() => {
		const currentText = line.text;
		if (prevText !== null && prevText !== currentText) {
			announce(currentText, 'polite');
		}
		prevText = currentText;
	});
</script>

<div class="state-chip tone-{line.tone}" class:live={line.live}>
	{#if line.live}
		<span class="live-dot" aria-hidden="true"></span>
	{/if}
	<span class="state-text">{line.text}</span>
</div>

<style>
	.state-chip {
		display: inline-flex;
		align-items: center;
		gap: var(--s2);
		font-size: var(--fs-meta);
		line-height: var(--lh-tight);
		color: var(--muted);
	}
	.state-chip.tone-accent {
		color: var(--accent);
	}
	.state-chip.tone-warning {
		color: var(--warning);
	}
	.state-chip.tone-danger {
		color: var(--danger);
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
