import type { TodoItem, TodoList, TodoStatus } from '#lib/stores/todos.svelte.js';
import type { Stage } from '#lib/ui/stage-track.js';

export const TODO_STATUS_LABEL: Record<TodoStatus, string> = {
	pending: 'ожидает',
	in_progress: 'в работе',
	completed: 'готово'
};

/** Столько точек дорожки помещается рядом со счётчиком и текстом на телефоне. */
const POINTS = 5;

export interface TodoView {
	done: number;
	total: number;
	allDone: boolean;
	/** Пункт в работе, иначе ближайший ожидающий. */
	current: TodoItem | null;
	stages: Stage[];
	index: number;
}

export function todoView(list: TodoList): TodoView {
	const { items, omitted } = list;
	const done = items.filter((item) => item.status === 'completed').length;
	const total = items.length + omitted;
	const allDone = done === total;
	const current =
		items.find((item) => item.status === 'in_progress') ??
		items.find((item) => item.status === 'pending') ??
		null;
	// Последняя точка — финиш: иначе дорожка считала бы последний пункт уже выполненным.
	const points = [...items.map((item) => item.text), 'Готово'];
	const start = Math.max(0, Math.min(done - 2, points.length - POINTS));
	return {
		done,
		total,
		allDone,
		current,
		stages: points
			.slice(start, start + POINTS)
			.map((label, k) => ({ id: String(start + k), label })),
		index: done - start
	};
}
