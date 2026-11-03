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
</script>

{#if running}
	<p class="compact" data-running="true"><span class="pulse" aria-hidden="true"></span><span>{text}</span></p>
{:else}
	<p class="compact" data-failed={compaction.ok === false ? 'true' : undefined}>{text}</p>
{/if}

<style>
	.compact {
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
	.pulse {
		flex: none;
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--accent);
		animation: compact-pulse var(--duration-stage-pulse) var(--ease-stage-pulse) infinite;
	}
	@keyframes compact-pulse {
		0%,
		100% {
			opacity: 0.45;
		}
		50% {
			opacity: 1;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.pulse {
			animation: none;
			opacity: 1;
		}
	}
</style>
