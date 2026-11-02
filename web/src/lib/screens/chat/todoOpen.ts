const KEY = 'canterlot.todo-open';

/** Явный выбор владельца по панели задач: null — ещё не выбирал, по умолчанию свёрнуто. */
export function readTodoOpen(): boolean | null {
	try {
		const raw = localStorage.getItem(KEY);
		const parsed = raw ? (JSON.parse(raw) as unknown) : null;
		return typeof parsed === 'boolean' ? parsed : null;
	} catch {
		return null;
	}
}

export function writeTodoOpen(open: boolean): void {
	localStorage.setItem(KEY, JSON.stringify(open));
}
