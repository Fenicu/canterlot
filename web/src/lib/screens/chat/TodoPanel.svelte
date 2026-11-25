<script lang="ts">
	import { slide } from 'svelte/transition';
	import { fmt } from '#lib/format/ru.js';
	import { durationMs } from '#lib/motion.js';
	import type { ChatStore } from '#lib/stores/chat.svelte.js';
	import SlimStrip from '#lib/ui/SlimStrip.svelte';
	import type { TodoStatus } from '#lib/stores/todos.svelte.js';
	import { TODO_STATUS_LABEL, todoView } from './todo';
	import { readTodoOpen, writeTodoOpen } from './todoOpen';

	let { chat, lane = null }: { chat: ChatStore; lane?: string | null } = $props();

	const listId = $props.id();
	const list = $derived(chat.todos.lane(lane));
	const shown = $derived(list && list.items.length > 0 ? list : null);
	const view = $derived(shown ? todoView(shown) : null);
	// Движение отражает настоящую работу: между ходами активная точка не пульсирует.
	const working = $derived(chat.activity?.state === 'working');

	/** Тон слова в строке — тот же, что у слова пункта в раскрытом списке. */
	const WORD_TONE: Record<TodoStatus, 'accent' | 'success' | 'warning' | 'muted'> = {
		in_progress: 'accent',
		completed: 'success',
		cancelled: 'warning',
		pending: 'muted'
	};

	function omittedNote(count: number): string {
		return `Ещё ${count} ${fmt.plural(count, ['пункт', 'пункта', 'пунктов'])} не поместилось`;
	}

	let chosen = $state<boolean | null>(readTodoOpen());
	const expanded = $derived(chosen ?? false);

	// Черта проводится только у пункта, который закрылся на глазах: открытый список показывает готовые сразу зачёркнутыми.
	function settle(node: HTMLElement) {
		const frame = requestAnimationFrame(() => node.setAttribute('data-settled', ''));
		return { destroy: () => cancelAnimationFrame(frame) };
	}

	function toggle() {
		chosen = !expanded;
		writeTodoOpen(chosen);
	}
</script>

{#if shown && view}
	<section class="todo" aria-label="Список задач агента">
		<div class="panel">
			<!-- Свёрнутый список — та же тонкая строка, что полоса плана; касание раскрывает пункты. -->
			<SlimStrip
				progress={view.total > 0 ? (view.done / view.total) * 100 : 0}
				count={`${view.done}/${view.total}`}
				word={TODO_STATUS_LABEL[view.headlineStatus]}
				text={view.headline}
				wordTone={WORD_TONE[view.headlineStatus]}
				gaugeLabel="Выполнение задач"
				label={`Задачи агента: готово ${view.done} из ${view.total}. ${TODO_STATUS_LABEL[view.headlineStatus]}: ${view.headline}`}
				{expanded}
				controls={listId}
				onclick={toggle}
			/>
			{#if expanded}
				<div transition:slide={{ duration: durationMs('--duration-expand') }}>
					<!-- Длинный список прокручивается: без фокуса клавиатуре до него не добраться. -->
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<ol class="items" id={listId} tabindex="0" data-live={working ? '' : undefined} use:settle>
						{#each shown.items as item, position (position)}
							<li class="item" data-status={item.status}>
								<span class="mark" aria-hidden="true"></span>
								<span class="text">{item.text}{#if item.status === 'completed'}<span class="strike" aria-hidden="true">{item.text}</span>{/if}</span>
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
		border-radius: var(--radius-sm);
		background: var(--surface);
		overflow: hidden;
	}

	.word {
		flex: none;
		font-size: var(--fs-sm);
		color: var(--muted);
		transition: color var(--duration-todo-strike) var(--ease-todo-strike);
	}

	.word[data-status='in_progress'] {
		color: var(--accent-link);
	}

	.word[data-status='completed'] {
		color: var(--success);
	}

	.word[data-status='cancelled'] {
		color: var(--warning);
	}

	.items {
		display: flex;
		flex-direction: column;
		gap: var(--s3);
		max-height: var(--todo-list-max);
		margin: var(--s0);
		padding: var(--s1) var(--s6) var(--s4);
		overflow-x: hidden;
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
		transition: color var(--duration-todo-strike) var(--ease-todo-strike);
	}

	.item:is([data-status='completed'], [data-status='cancelled']) {
		color: var(--secondary);
	}

	.text {
		position: relative;
		overflow-wrap: anywhere;
	}

	/* Черта лежит поверх текста его же копией: перенос строк совпадает, буквы копии не рисуются, видна только линия. */
	.strike {
		position: absolute;
		inset: var(--s0);
		-webkit-text-fill-color: var(--transparent);
		text-decoration-line: line-through;
		text-decoration-color: var(--secondary);
		text-decoration-thickness: var(--hairline);
		clip-path: inset(var(--s0));
		pointer-events: none;
		user-select: none;
		transition: clip-path var(--duration-todo-strike) var(--ease-todo-strike);
	}

	@starting-style {
		.items:global([data-settled]) .strike {
			clip-path: inset(var(--s0) 100% var(--s0) var(--s0));
		}
	}

	/* Пустая строка текста даёт ячейке базовую линию: точка стоит у первой строки пункта.
	   Пульс снимается плавно: точка возвращает полную яркость, а не вспыхивает. */
	.mark {
		display: flex;
		align-items: center;
		transition: opacity var(--duration-todo-strike) var(--ease-todo-strike);
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
		transition: background-color var(--duration-todo-strike) var(--ease-todo-strike);
	}

	.item[data-status='in_progress'] .mark::after {
		background: var(--accent);
	}

	/* Готовая точка берёт цвет слова «готово»: смена тона видна и без подписи. */
	.item[data-status='completed'] .mark::after {
		background: var(--success);
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
