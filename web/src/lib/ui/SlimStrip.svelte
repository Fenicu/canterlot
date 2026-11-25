<script lang="ts">
	import Gauge from './Gauge.svelte';
	import Icon from './Icon.svelte';

	/** Цвет слова состояния; полоска красится только тревожным тоном. */
	type WordTone = 'accent' | 'success' | 'warning' | 'muted';

	let {
		progress,
		count,
		word,
		text,
		wordTone = 'accent',
		warning = false,
		gaugeLabel,
		label,
		title,
		expanded,
		controls,
		onclick
	}: {
		/** Готовая доля, 0..100. */
		progress: number;
		/** «1/4». */
		count: string;
		word: string;
		text: string;
		wordTone?: WordTone;
		/** Полоска цветом предупреждения. */
		warning?: boolean;
		gaugeLabel: string;
		label?: string;
		title?: string;
		/** Раскрывающая строка: стрелка поворачивается вниз. Без значения строка просто ведёт дальше. */
		expanded?: boolean;
		controls?: string;
		onclick: () => void;
	} = $props();
</script>

<!-- Тонкая строка над полем ввода: полоска готовности, счёт, слово состояния с текстом и стрелка. -->
<button
	type="button"
	class="slim"
	aria-label={label}
	{title}
	aria-expanded={expanded}
	aria-controls={controls}
	{onclick}
>
	<span class="bar" aria-hidden="true"><Gauge value={progress} tone={warning ? 'warning' : 'normal'} label={gaugeLabel} /></span>
	<span class="count" data-role="strip-count">{count}</span>
	<span class="summary" data-role="strip-task"><span class="word" data-tone={wordTone}>{word}</span> {text}</span>
	<span class="chevron" class:open={expanded}><Icon name="chevron" /></span>
</button>

<style>
	.slim {
		display: flex;
		align-items: center;
		gap: var(--s4);
		width: 100%;
		min-width: var(--s0);
		min-height: var(--strip-slim);
		padding: var(--s0) var(--s3) var(--s0) var(--s6);
		border: var(--s0);
		border-radius: inherit;
		background: var(--transparent);
		color: var(--secondary);
		font-size: var(--fs-sm);
		line-height: var(--lh-tight);
		text-align: left;
		white-space: nowrap;
	}
	.slim:focus-visible {
		outline: var(--focus-width) solid var(--accent);
		outline-offset: calc(var(--focus-width) * -1);
	}
	.bar {
		display: flex;
		flex: none;
	}
	.bar :global([role='meter']) {
		width: var(--s14);
		height: var(--s2);
	}
	.count {
		flex: none;
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.summary {
		flex: 1;
		min-width: var(--s0);
		overflow: hidden;
		color: var(--text);
		text-overflow: ellipsis;
	}
	.word {
		color: var(--accent-link);
		transition: color var(--duration-todo-strike) var(--ease-todo-strike);
	}
	.word[data-tone='success'] { color: var(--success); }
	.word[data-tone='warning'] { color: var(--warning); }
	.word[data-tone='muted'] { color: var(--muted); }
	.chevron {
		display: flex;
		flex: none;
		color: var(--muted);
		transition: transform var(--duration-fast) var(--ease);
	}
	.chevron.open {
		transform: rotate(90deg);
	}
	.chevron :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}
	@media (hover: hover) and (pointer: fine) {
		.slim:hover {
			background: var(--hover);
		}
	}
</style>
