import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import TodoPanel from '#lib/screens/chat/TodoPanel.svelte';
import type { ChatStore } from '#lib/stores/chat.svelte.js';
import type { TodoList, TodoStatus } from '#lib/stores/todos.svelte.js';

function chatWith(items: [string, TodoStatus][], omitted = 0): ChatStore {
	const list: TodoList = { lane_block_id: null, revision: 1, omitted, items: items.map(([text, status]) => ({ text, status })) };
	return {
		id: 'conv-1',
		activity: { state: 'working', evidence: { phase: 'running_tool' } },
		todos: { lane: () => list }
	} as unknown as ChatStore;
}

describe('chat/TodoPanel', () => {
	let instance: ReturnType<typeof mount> | undefined;

	beforeEach(() => localStorage.clear());

	afterEach(() => {
		if (instance) void unmount(instance);
		instance = undefined;
		document.body.textContent = '';
		localStorage.clear();
	});

	function show(chat: ChatStore): HTMLElement {
		const target = document.createElement('div');
		document.body.appendChild(target);
		instance = mount(TodoPanel, { target, props: { chat } });
		flushSync();
		return target;
	}

	const owners = (): [string, TodoStatus][] => [
		['Codex 0.172.0: schema check', 'in_progress'],
		['OpenCode 2.4.1: манифест', 'pending'],
		['Claude Code 2.1.332: манифест', 'pending'],
		['Сводка для оркестратора', 'pending']
	];

	it('collapsed_list_is_one_slim_line_with_count_word_and_task', () => {
		const target = show(chatWith(owners()));
		const panel = target.querySelector<HTMLElement>('[aria-label="Список задач агента"]')!;
		expect(panel.querySelectorAll('button')).toHaveLength(1);
		expect(panel.querySelector('[data-role="strip-count"]')?.textContent).toBe('0/4');
		expect(panel.querySelector('[data-role="strip-task"]')?.textContent).toBe('в работе Codex 0.172.0: schema check');
		expect(panel.querySelector('[data-role="strip-task"] .word')?.getAttribute('data-tone')).toBe('accent');
		expect(panel.querySelector('[role="meter"]')?.getAttribute('aria-valuenow')).toBe('0');
		expect(panel.querySelector('ol')).toBeNull();
		// Дорожки с точками больше нет: её место заняла полоска готовности.
		expect(panel.querySelector('.st')).toBeNull();
	});

	it('tap_expands_the_list_below_the_line_and_folds_it_back', () => {
		const target = show(chatWith(owners()));
		const button = target.querySelector<HTMLButtonElement>('button')!;
		expect(button.getAttribute('aria-expanded')).toBe('false');
		button.click();
		flushSync();
		expect(button.getAttribute('aria-expanded')).toBe('true');
		const list = target.querySelector<HTMLElement>('ol')!;
		expect(button.getAttribute('aria-controls')).toBe(list.id);
		expect(list.querySelectorAll('li')).toHaveLength(4);
		expect(localStorage.getItem('canterlot.todo-open')).toBe('true');
		button.click();
		flushSync();
		expect(button.getAttribute('aria-expanded')).toBe('false');
		expect(localStorage.getItem('canterlot.todo-open')).toBe('false');
	});

	it('progress_and_word_follow_the_list', () => {
		const target = show(chatWith([['Схема', 'completed'], ['Манифест', 'completed'], ['Сводка', 'cancelled'], ['Отчёт', 'pending']]));
		expect(target.querySelector('[data-role="strip-count"]')?.textContent).toBe('2/3');
		expect(target.querySelector('[data-role="strip-task"]')?.textContent).toBe('ожидает Отчёт');
		expect(target.querySelector('[data-role="strip-task"] .word')?.getAttribute('data-tone')).toBe('muted');
		expect(Number(target.querySelector('[role="meter"]')?.getAttribute('aria-valuenow'))).toBeCloseTo(66.67, 1);
	});

	it('finished_list_says_so_in_the_success_tone', () => {
		const target = show(chatWith([['Схема', 'completed'], ['Манифест', 'completed']]));
		expect(target.querySelector('[data-role="strip-count"]')?.textContent).toBe('2/2');
		expect(target.querySelector('[data-role="strip-task"]')?.textContent).toBe('готово Все пункты выполнены');
		expect(target.querySelector('[data-role="strip-task"] .word')?.getAttribute('data-tone')).toBe('success');
	});
});
