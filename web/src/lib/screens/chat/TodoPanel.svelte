<script lang="ts">
	import { slide } from 'svelte/transition';
	import { fmt } from '#lib/format/ru.js';
	import { durationMs } from '#lib/motion.js';
	import { pane } from '#lib/shell/pane.svelte.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import Icon from '#lib/ui/Icon.svelte';
	import StageTrack from '#lib/ui/StageTrack.svelte';
	import { TODO_STATUS_LABEL, todoView } from './todo';

	let { chat, lane = null }: { chat: ChatStore; lane?: string | null } = $props();

	const listId = $props.id();
	const list = $derived(chat.todos.lane(lane));
	const shown = $derived(list && list.items.length > 0 ? list : null);
	const view = $derived(shown ? todoView(shown) : null);
	// Движение отражает настоящую работу: между ходами активная точка не пульсирует.
	const working = $derived(chat.activity?.state === 'working');
	const word = $derived(view?.current ? TODO_STATUS_LABEL[view.current.status] : TODO_STATUS_LABEL.completed);

	function omittedNote(count: number): string {
		return `Ещё ${count} ${fmt.plural(count, ['пункт', 'пункта', 'пунктов'])} не поместилось`;
	}

	let chosen = $state<boolean | null>(null);
	const expanded = $derived(chosen ?? pane.desktop);
</script>

{#if shown && view}
	<section class="todo" aria-label="Список задач агента">
		<div class="panel">
			<div class="head">
				<StageTrack
					stages={view.stages}
					index={view.index}
					state={view.allDone || working ? 'active' : 'held'}
					label="Выполнение задач"
					status={`${view.done} из ${view.total}`}
				/>
				<button
					type="button"
					class="toggle"
					aria-expanded={expanded}
					aria-controls={listId}
					onclick={() => (chosen = !expanded)}
				>
					<span class="word" data-status={view.current?.status ?? 'completed'}>{word}</span>
					<span class="now">{view.current?.text ?? 'Все пункты выполнены'}</span>
					<span class="chevron" class:open={expanded}><Icon name="chevron" /></span>
				</button>
			</div>
			{#if expanded}
				<div transition:slide={{ duration: durationMs('--duration-expand') }}>
					<!-- Длинный список прокручивается: без фокуса клавиатуре до него не добраться. -->
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<ol class="items" id={listId} tabindex="0" data-live={working ? '' : undefined}>
						{#each shown.items as item, position (position)}
							<li class="item" data-status={item.status}>
								<span class="mark" aria-hidden="true"></span>
								<span class="text">{item.text}</span>
								<span class="word" data-status={item.status}>{TODO_STATUS_LABEL[item.status]}</span>
							</li>
						{/each}
						{#if shown.omitted > 0}
							<li class="omitted">{omittedNote(shown.omitted)}</li>
						{/if}
					</ol>
				</div>
			{/if}
		</div>
	</section>
{/if}

<style>
	.todo {
		box-sizing: border-box;
		padding: var(--s2) var(--gutter) var(--s0);
	}

	.panel {
		border-radius: var(--radius-card);
		background: var(--surface);
		overflow: hidden;
	}

	.head {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--s4);
		min-height: var(--target);
		padding-inline: var(--s6) var(--s3);
	}

	.head :global(.st) {
		flex: none;
	}

	.toggle {
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--s3);
		min-width: var(--s0);
		min-height: var(--target);
		padding: var(--s0);
		background: var(--transparent);
		color: var(--text);
		font-size: var(--fs-sm);
		text-align: start;
	}

	/* Касание дорожки тоже раскрывает список: кнопка растянута на всю шапку. */
	.toggle::after {
		content: '';
		position: absolute;
		inset: var(--s0);
	}

	.toggle:focus-visible {
		outline: none;
	}

	.toggle:focus-visible::after {
		outline: var(--focus-width) solid var(--accent);
		outline-offset: calc(var(--focus-width) * -1);
		border-radius: var(--radius-card);
	}

	.word {
		flex: none;
		font-size: var(--fs-sm);
		color: var(--muted);
		transition: color var(--duration-fast) var(--ease);
	}

	.word[data-status='in_progress'] {
		color: var(--accent-link);
	}

	.word[data-status='completed'] {
		color: var(--success);
	}

	.now {
		flex: 1;
		min-width: var(--s0);
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
	}

	.chevron {
		flex: none;
		display: grid;
		place-items: center;
		color: var(--muted);
		transition: transform var(--duration-fast) var(--ease);
	}

	.chevron :global(svg) {
		width: var(--icon-small);
		height: var(--icon-small);
	}

	.chevron.open {
		transform: rotate(90deg);
	}

	.items {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		max-height: var(--todo-list-max);
		margin: var(--s0);
		padding: var(--s1) var(--s6) var(--s4);
		overflow-y: auto;
		list-style: none;
	}

	.item {
		display: grid;
		grid-template-columns: var(--mark) minmax(var(--s0), 1fr) auto;
		align-items: baseline;
		gap: var(--s4);
		font-size: var(--fs-sm);
		color: var(--text);
		transition: color var(--duration-fast) var(--ease);
	}

	.item[data-status='completed'] {
		color: var(--secondary);
	}

	.text {
		overflow-wrap: anywhere;
	}

	/* Пустая строка текста даёт ячейке базовую линию: точка стоит у первой строки пункта. */
	.mark {
		display: flex;
		align-items: center;
	}

	.mark::before {
		content: '\200b';
	}

	.mark::after {
		content: '';
		flex: none;
		width: var(--mark);
		height: var(--mark);
		border-radius: var(--radius-round);
		background: var(--badge);
		transition: background-color var(--duration-fast) var(--ease);
	}

	.item:is([data-status='in_progress'], [data-status='completed']) .mark::after {
		background: var(--accent);
	}

	.items[data-live] .item[data-status='in_progress'] .mark {
		animation: todo-pulse var(--duration-stage-pulse) var(--ease-stage-pulse) infinite;
	}

	@keyframes todo-pulse {
		50% {
			opacity: var(--opacity-stage-halo);
		}
	}

	.omitted {
		padding-inline-start: calc(var(--mark) + var(--s4));
		font-size: var(--fs-sm);
		color: var(--muted);
	}
</style>
