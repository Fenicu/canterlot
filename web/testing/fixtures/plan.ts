import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation, seedHistory } from '../fake-hub/seed';
import type { PlanTask } from '../../src/lib/stores/plan.svelte';

const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const at = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

export const REPORT_TEXT = 'Отчёт: хранилище плана готово, тесты зелёные';

export interface PlanSeed {
	orchestrator: string;
	executors: { working: string; review: string; failed: string; accepted: string };
	reportBlockId: string;
	tasks: PlanTask[];
}

/**
 * Оркестратор с планом во всех состояниях: исполнитель работает, ждёт проверки, упал,
 * принят; одна задача ждёт без исполнителя, одна отменена. У задачи на проверке есть
 * коммиты и отчёт глубоко в ленте исполнителя.
 */
export function seedPlan(api: FakeHubApi, opts: { reportAt?: number; history?: number } = {}): PlanSeed {
	const orchestrator = seedConversation(api, { title: 'Доска плана', harness: CLAUDE, model: 'opus-5.5', last_activity_at: at(1) });
	const child = (title: string, model: string, activity: Record<string, unknown>) =>
		seedConversation(api, { title, parent_id: orchestrator, spawn_depth: 1, harness: CLAUDE, model, activity: activity as never, last_activity_at: at(3) });
	const working = child('Миграция plan_tasks', 'opus-5.5', { state: 'working', evidence: { kind: 'working', phase: 'generating' }, since: at(4) });
	const review = child('Хранилище плана в PWA', 'sonnet-5', { state: 'idle', evidence: { kind: 'idle', last_turn_at: at(2) }, since: at(2) });
	const failed = child('Отбор подписок', 'haiku', { state: 'failed', evidence: { kind: 'failed', reason: 'process_exited' }, since: at(9) });
	const accepted = child('Схема событий плана', 'opus-5.5', { state: 'stopped', evidence: { kind: 'stopped', by: 'owner' }, since: at(30) });

	const history = seedHistory(api, review, opts.history ?? 150);
	const report = history[(opts.reportAt ?? 20) - 1]!;
	report.content = REPORT_TEXT;
	report.content_bytes = REPORT_TEXT.length;
	api.blocks.set(report.id, { text: REPORT_TEXT, rev: 1, state: 'final' });

	const tasks: PlanTask[] = [];
	const push = (task: Partial<PlanTask> & Pick<PlanTask, 'title'>) => {
		tasks.push(api.pushPlanTask({ orchestrator_id: orchestrator, created_at: at(60), updated_at: at(5), ...task }));
	};
	push({ title: 'Таблица plan_tasks и миграция', state: 'in_progress', executor_id: working, attempts: 1, note: 'Номер задачи на оркестратор, без повторного использования.' });
	push({ title: 'Хранилище плана и вкладка «План»', state: 'in_review', executor_id: review, attempts: 1, findings: ['Снимок заменяет набор задач целиком', 'Ревизия защищает от старых событий'] });
	push({ title: 'Перевод в in_review по подписке', state: 'blocked', executor_id: failed, attempts: 2, blocker: 'Нужен общий ответ гейта толчка, иначе толчок уйдёт дважды' });
	push({ title: 'Документация API', state: 'waiting' });
	push({ title: 'Схема событий plan_task_changed', state: 'done', executor_id: accepted, attempts: 1 });
	push({ title: 'Черновик дизайна доски', state: 'done' });
	push({ title: 'Вкладка в нижней панели', state: 'cancelled', note: 'Пятой вкладки не будет.' });

	const reviewTask = tasks[1]!;
	api.planDetails.set(reviewTask.id, {
		commits: [
			{ sha: 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678', subject: 'Добавить хранилище плана с ревизиями' },
			{ sha: 'b2c3d4e5f60718293a4b5c6d7e8f90123456789a', subject: 'Показывать вкладку «План» в панели делегирования' },
			{ sha: 'c3d4e5f60718293a4b5c6d7e8f90123456789ab0', subject: 'Проверить план на телефоне и столе' }
		],
		commits_truncated: false,
		files_changed: 14,
		report_block_id: report.id
	});
	api.planDetails.set(tasks[0]!.id, {
		commits: [{ sha: 'd4e5f60718293a4b5c6d7e8f90123456789abc01', subject: 'Создать таблицу plan_tasks' }],
		commits_truncated: false,
		files_changed: 3,
		report_block_id: null
	});
	return { orchestrator, executors: { working, review, failed, accepted }, reportBlockId: report.id, tasks };
}
