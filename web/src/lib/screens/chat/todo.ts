import type { TodoItem, TodoList, TodoStatus } from '#lib/stores/todos.svelte.js';
import type { Stage } from '#lib/ui/stage-track.js';

export const TODO_STATUS_LABEL: Record<TodoStatus, string> = {
	pending: 'ожидает',
	in_progress: 'в работе',
	completed: 'готово',
	cancelled: 'отменено'
};

/** Столько точек дорожки помещается рядом со счётчиком и текстом на телефоне. */
const POINTS = 5;

export interface TodoView {
	done: number;
	total: number;
	allDone: boolean;
	/** Пункт в работе, иначе ближайший ожидающий. */
	current: TodoItem | null;
	/** Строка свёрнутой панели. */
	headline: string;
	headlineStatus: TodoStatus;
	stages: Stage[];
	index: number;
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
	// Последняя точка — финиш: иначе дорожка считала бы последний пункт уже выполненным.
	const points = [...items.map((item) => item.text), 'Готово'];
	const start = Math.max(0, Math.min(done - 2, points.length - POINTS));
	return {
		done,
		total,
		allDone,
		current,
		headline,
		headlineStatus,
		stages: points
			.slice(start, start + POINTS)
			.map((label, k) => ({ id: String(start + k), label })),
		index: done - start
	};
}
