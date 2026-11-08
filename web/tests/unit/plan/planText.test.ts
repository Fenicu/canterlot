import { describe, expect, it } from 'vitest';
import {
	EXECUTOR_GONE,
	commitsText,
	countText,
	executorView,
	filesText,
	groupTasks,
	planBadge,
	planStrip,
	planSummary,
	shortSha,
	taskTitle
} from '#lib/screens/plan/planText.js';
import type { ListRow } from '#lib/stores/list.svelte.js';
import type { PlanTask, PlanTaskState } from '#lib/stores/plan.svelte.js';

let serial = 0;
function task(state: PlanTaskState, patch: Partial<PlanTask> = {}): PlanTask {
	serial += 1;
	return {
		id: `t${serial}`,
		orchestrator_id: 'o',
		number: serial,
		title: `Задача ${serial}`,
		note: null,
		blocker: null,
		findings: [],
		state,
		executor_id: null,
		attempts: 0,
		created_at: '2026-11-08T10:00:00.000Z',
		updated_at: '2026-11-08T10:00:00.000Z',
		revision: 1,
		...patch
	};
}

const row = (state: ListRow['activity']['state']): ListRow => ({ id: 'e', title: 'Исполнитель', activity: { state } }) as ListRow;

describe('plan text', () => {
	it('groups_follow_attention_order_and_fold_closed_states', () => {
		const groups = groupTasks([task('done'), task('waiting'), task('cancelled'), task('blocked'), task('in_review'), task('in_progress'), task('in_progress')]);
		expect(groups.map((group) => [group.state, group.title, group.tasks.length, group.folded])).toEqual([
			['in_progress', 'В работе', 2, false],
			['in_review', 'На проверке', 1, false],
			['blocked', 'Заблокировано', 1, false],
			['waiting', 'Ждёт', 1, false],
			['done', 'Готово', 1, true],
			['cancelled', 'Отменено', 1, true]
		]);
		expect(groupTasks([task('waiting')]).map((group) => group.state)).toEqual(['waiting']);
		const sorted = groupTasks([task('waiting', { number: 9 }), task('waiting', { number: 2 })])[0]!.tasks.map((item) => item.number);
		expect(sorted).toEqual([2, 9]);
	});

	it('counts_agree_with_numbers', () => {
		expect(countText('blocked', 1)).toBe('1 заблокирована');
		expect(countText('blocked', 3)).toBe('3 заблокированы');
		expect(countText('blocked', 5)).toBe('5 заблокировано');
		expect(countText('waiting', 1)).toBe('1 ждёт');
		expect(countText('waiting', 21)).toBe('21 ждёт');
		expect(countText('waiting', 2)).toBe('2 ждут');
		expect(countText('cancelled', 2)).toBe('2 отменены');
		expect(countText('cancelled', 11)).toBe('11 отменено');
		expect(countText('in_review', 4)).toBe('4 на проверке');
		expect(commitsText(1)).toBe('1 коммит');
		expect(commitsText(3)).toBe('3 коммита');
		expect(commitsText(50, true)).toBe('более 50 коммитов');
		expect(filesText(1)).toBe('1 файл изменён');
		expect(filesText(14)).toBe('14 файлов изменено');
		expect(filesText(2)).toBe('2 файла изменены');
	});

	it('summary_lists_non_empty_states_in_order', () => {
		expect(planSummary([task('done'), task('in_progress'), task('in_progress'), task('in_review'), task('done')])).toBe('2 в работе · 1 на проверке · 2 готово');
		expect(planSummary([])).toBe('');
	});

	it('strip_appears_only_for_review_or_blocked_and_warns_on_blocked', () => {
		expect(planStrip([task('in_progress'), task('waiting'), task('done')])).toBeNull();
		expect(planStrip([task('in_review'), task('in_progress')])).toEqual({ text: 'План: 1 на проверке', tone: 'default' });
		expect(planStrip([task('in_review'), task('blocked'), task('blocked')])).toEqual({ text: 'План: 2 заблокированы · 1 на проверке', tone: 'warning' });
	});

	it('badge_counts_done_out_of_not_cancelled', () => {
		expect(planBadge([])).toBeNull();
		const badge = planBadge([task('done'), task('done'), task('in_progress'), task('waiting'), task('blocked'), task('in_review'), task('in_review'), task('cancelled')]);
		expect(badge).toEqual({ text: 'План · 2/7', label: 'План: готово 2 из 7' });
	});

	it('executor_words_cover_live_states_and_deletion', () => {
		const lookup = (state: ListRow['activity']['state']) => () => row(state);
		const none = () => false;
		expect(executorView({ executor_id: 'e', attempts: 1 }, lookup('working'), none)).toMatchObject({ word: 'работает', tone: 'accent' });
		expect(executorView({ executor_id: 'e', attempts: 1 }, lookup('waiting_for_user'), none).word).toBe('ждёт ответа');
		expect(executorView({ executor_id: 'e', attempts: 1 }, lookup('failed'), none)).toMatchObject({ word: 'упала', tone: 'danger' });
		expect(executorView({ executor_id: 'e', attempts: 1 }, lookup('stopped'), none).word).toBe('остановлена');
		// Хаб снял ссылку после удаления исполнителя.
		expect(executorView({ executor_id: null, attempts: 1 }, () => undefined, none)).toMatchObject({ row: null, word: EXECUTOR_GONE });
		// Список узнал об удалении раньше, чем пришло событие задачи.
		expect(executorView({ executor_id: 'e', attempts: 1 }, () => undefined, () => true).word).toBe(EXECUTOR_GONE);
		// Исполнителя не было.
		expect(executorView({ executor_id: null, attempts: 0 }, () => undefined, none)).toMatchObject({ row: null, word: null });
	});

	it('title_and_sha_are_short', () => {
		expect(taskTitle({ number: 3, title: 'Название' })).toBe('#3 Название');
		expect(shortSha('a1b2c3d4e5f6')).toBe('a1b2c3d');
	});
});
