import { fmt } from '#lib/format/ru.js';
import type { ListActivity, ListRow } from '#lib/stores/list.svelte.js';
import type { PlanTask, PlanTaskState } from '#lib/stores/plan.svelte.js';

export type PlanTone = 'accent' | 'blue' | 'warning' | 'success' | 'muted' | 'danger' | 'default';

/** Порядок групп на экране плана: то, что требует внимания, выше. */
export const STATE_ORDER: readonly PlanTaskState[] = ['in_progress', 'in_review', 'blocked', 'waiting', 'done', 'cancelled'];

/** Закрытые группы свёрнуты, пока их не раскроют. */
export const FOLDED_STATES: ReadonlySet<PlanTaskState> = new Set(['done', 'cancelled']);

/** Заголовок группы. */
export const GROUP_TITLE: Record<PlanTaskState, string> = {
	in_progress: 'В работе',
	in_review: 'На проверке',
	blocked: 'Заблокировано',
	waiting: 'Ждёт',
	done: 'Готово',
	cancelled: 'Отменено'
};

/** Слово состояния у строки задачи: задача женского рода. */
export const STATE_WORD: Record<PlanTaskState, string> = {
	in_progress: 'в работе',
	in_review: 'на проверке',
	blocked: 'заблокирована',
	waiting: 'ждёт',
	done: 'готова',
	cancelled: 'отменена'
};

export const STATE_TONE: Record<PlanTaskState, PlanTone> = {
	in_progress: 'accent',
	in_review: 'blue',
	blocked: 'warning',
	waiting: 'muted',
	done: 'success',
	cancelled: 'muted'
};

/** Число задач в состоянии: «2 в работе», «3 заблокированы», «5 ждут», «4 готово». */
export function countText(state: PlanTaskState, n: number): string {
	switch (state) {
		case 'in_progress':
			return `${n} в работе`;
		case 'in_review':
			return `${n} на проверке`;
		case 'blocked':
			return `${n} ${fmt.plural(n, ['заблокирована', 'заблокированы', 'заблокировано'])}`;
		case 'waiting':
			return `${n} ${fmt.plural(n, ['ждёт', 'ждут', 'ждут'])}`;
		case 'done':
			return `${n} готово`;
		case 'cancelled':
			return `${n} ${fmt.plural(n, ['отменена', 'отменены', 'отменено'])}`;
	}
}

export function countByState(tasks: readonly PlanTask[]): Record<PlanTaskState, number> {
	const counts: Record<PlanTaskState, number> = { in_progress: 0, in_review: 0, blocked: 0, waiting: 0, done: 0, cancelled: 0 };
	for (const task of tasks) counts[task.state] += 1;
	return counts;
}

export interface PlanGroup<T extends PlanTask = PlanTask> {
	state: PlanTaskState;
	title: string;
	tasks: T[];
	folded: boolean;
}

/** Группы по состоянию в порядке экрана, пустые не показываются; внутри — по номеру. */
export function groupTasks<T extends PlanTask>(tasks: readonly T[]): PlanGroup<T>[] {
	return STATE_ORDER.map((state) => ({
		state,
		title: GROUP_TITLE[state],
		tasks: tasks.filter((task) => task.state === state).sort((a, b) => a.number - b.number),
		folded: FOLDED_STATES.has(state)
	})).filter((group) => group.tasks.length > 0);
}

/** Сводка плана по всем непустым состояниям: «2 в работе · 1 на проверке · 4 готово». */
export function planSummary(tasks: readonly PlanTask[]): string {
	const counts = countByState(tasks);
	return STATE_ORDER.filter((state) => counts[state] > 0)
		.map((state) => countText(state, counts[state]))
		.join(' · ');
}

/**
 * Полоса в чате оркестратора: только когда есть задачи на проверке или заблокированные.
 * Заблокированные идут первыми и красят полосу тоном предупреждения.
 */
export function planStrip(tasks: readonly PlanTask[]): { text: string; tone: 'warning' | 'default' } | null {
	const counts = countByState(tasks);
	if (counts.blocked === 0 && counts.in_review === 0) return null;
	const parts: string[] = [];
	if (counts.blocked > 0) parts.push(countText('blocked', counts.blocked));
	if (counts.in_review > 0) parts.push(countText('in_review', counts.in_review));
	return { text: `План: ${parts.join(' · ')}`, tone: counts.blocked > 0 ? 'warning' : 'default' };
}

/** Метка строки сессии: готово из всех, кроме отменённых. */
export function planBadge(tasks: readonly PlanTask[]): { text: string; label: string } | null {
	if (tasks.length === 0) return null;
	const counts = countByState(tasks);
	const total = tasks.length - counts.cancelled;
	return {
		text: `План · ${counts.done}/${total}`,
		label: `План: готово ${counts.done} из ${total}`
	};
}

/** Номер и название: «#3 Название». */
export function taskTitle(task: Pick<PlanTask, 'number' | 'title'>): string {
	return `#${task.number} ${task.title}`;
}

export function commitsText(n: number, truncated = false): string {
	return `${truncated ? 'более ' : ''}${n} ${fmt.plural(n, ['коммит', 'коммита', 'коммитов'])}`;
}

export function filesText(n: number): string {
	return `${n} ${fmt.plural(n, ['файл', 'файла', 'файлов'])} ${fmt.plural(n, ['изменён', 'изменены', 'изменено'])}`;
}

export interface ExecutorView {
	/** Строка списка исполнителя; `null` — исполнителя нет или он удалён. */
	row: ListRow | null;
	/** Слово состояния исполнителя; `null` — у задачи не было исполнителя. */
	word: string | null;
	tone: PlanTone;
}

const ACTIVITY_WORD: Record<ListActivity['state'], { word: string; tone: PlanTone }> = {
	working: { word: 'работает', tone: 'accent' },
	waiting_for_user: { word: 'ждёт ответа', tone: 'warning' },
	failed: { word: 'упала', tone: 'danger' },
	stopped: { word: 'остановлена', tone: 'muted' },
	idle: { word: 'закончила ход', tone: 'muted' },
	stalled: { word: 'подвисла', tone: 'warning' },
	quota_paused: { word: 'ждёт лимита', tone: 'warning' }
};

export const EXECUTOR_GONE = 'исполнитель удалён';

/**
 * Исполнитель задачи по общему списку сессий. Хаб снимает ссылку, когда исполнителя удалили,
 * а список узнаёт об удалении своим событием: оба случая читаются как «исполнитель удалён».
 */
export function executorView(
	task: Pick<PlanTask, 'executor_id' | 'attempts'>,
	lookup: (id: string) => ListRow | undefined,
	isDeleted: (id: string) => boolean
): ExecutorView {
	if (!task.executor_id) {
		return task.attempts > 0 ? { row: null, word: EXECUTOR_GONE, tone: 'muted' } : { row: null, word: null, tone: 'muted' };
	}
	const row = lookup(task.executor_id);
	if (!row) return isDeleted(task.executor_id) ? { row: null, word: EXECUTOR_GONE, tone: 'muted' } : { row: null, word: null, tone: 'muted' };
	const activity = ACTIVITY_WORD[row.activity.state] ?? ACTIVITY_WORD.idle;
	return { row, word: activity.word, tone: activity.tone };
}

/** Короткий хеш коммита: семь знаков, как у git. */
export function shortSha(sha: string): string {
	return sha.slice(0, 7);
}
