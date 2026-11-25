import type { TodoItem, TodoList, TodoStatus } from '#lib/stores/todos.svelte.js';

export const TODO_STATUS_LABEL: Record<TodoStatus, string> = {
	pending: 'ожидает',
	in_progress: 'в работе',
	completed: 'готово',
	cancelled: 'отменено'
};

export interface TodoView {
	done: number;
	total: number;
	allDone: boolean;
	/** Пункт в работе, иначе ближайший ожидающий. */
	current: TodoItem | null;
	/** Строка свёрнутой панели. */
	headline: string;
	headlineStatus: TodoStatus;
}

export function todoView(list: TodoList): TodoView {
	const { omitted } = list;
	// Отменённый пункт остаётся в списке, но работы по нему больше не будет.
	const items = list.items.filter((item) => item.status !== 'cancelled');
	const done = items.filter((item) => item.status === 'completed').length;
	const total = items.length + omitted;
	const allDone = done === total;
	const allCancelled = total === 0 && list.items.length > 0;
	const current =
		items.find((item) => item.status === 'in_progress') ??
		items.find((item) => item.status === 'pending') ??
		null;
	// Показанные пункты могут кончиться раньше списка: про отрезанный хвост ничего не известно.
	const headline =
		current?.text ??
		(allCancelled ? 'Все пункты отменены' : allDone ? 'Все пункты выполнены' : 'Остальные пункты не поместились');
	const headlineStatus = current?.status ?? (allCancelled ? 'cancelled' : allDone ? 'completed' : 'pending');
	return { done, total, allDone, current, headline, headlineStatus };
}
