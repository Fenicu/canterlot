import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import type { PlanTask, PlanTaskState } from '#lib/stores/plan.svelte.js';

let tasks: PlanTask[] = [];
vi.mock('#lib/app/runtime.js', () => ({
	getRuntime: () => ({ list: { plan: { of: () => tasks } } })
}));

const { default: PlanStrip } = await import('#lib/screens/plan/PlanStrip.svelte');

let serial = 0;
function task(state: PlanTaskState, title = `Задача ${serial + 1}`): PlanTask {
	serial += 1;
	return {
		id: `t${serial}`,
		orchestrator_id: 'o',
		number: serial,
		title,
		note: null,
		blocker: null,
		findings: [],
		state,
		executor_id: null,
		attempts: 0,
		created_at: '2026-11-08T10:00:00.000Z',
		updated_at: '2026-11-08T10:00:00.000Z',
		revision: 1,
		archived: false
	} as PlanTask;
}

describe('plan/PlanStrip', () => {
	let instance: ReturnType<typeof mount> | undefined;

	afterEach(() => {
		if (instance) void unmount(instance);
		instance = undefined;
		document.body.textContent = '';
		serial = 0;
	});

	function show(onopen = vi.fn()): HTMLElement {
		const target = document.createElement('div');
		document.body.appendChild(target);
		instance = mount(PlanStrip, { target, props: { orchestratorId: 'o', onopen } });
		flushSync();
		return target;
	}

	it('slim_line_shows_the_count_the_task_and_opens_the_plan', () => {
		tasks = [task('done'), task('in_review', 'Codex 0.172.0: schema check'), task('in_progress'), task('waiting')];
		const onopen = vi.fn();
		const target = show(onopen);
		const strip = target.querySelector<HTMLElement>('[data-role="plan-strip"]')!;
		expect(strip.dataset.tone).toBe('default');
		expect(strip.querySelector('[data-role="plan-strip-count"]')?.textContent).toBe('1/4');
		expect(strip.querySelector('[data-role="plan-strip-task"]')?.textContent).toBe('на проверке Codex 0.172.0: schema check');
		expect(strip.querySelector('[role="meter"]')?.getAttribute('aria-valuenow')).toBe('25');
		const button = strip.querySelector<HTMLButtonElement>('button')!;
		expect(button.title).toBe('План: 1 на проверке');
		button.click();
		expect(onopen).toHaveBeenCalledOnce();
	});

	it('blocked_task_takes_the_line_and_the_warning_tone', () => {
		tasks = [task('in_review', 'Проверка'), task('blocked', 'Ждёт ответа гейта'), task('cancelled')];
		const strip = show().querySelector<HTMLElement>('[data-role="plan-strip"]')!;
		expect(strip.dataset.tone).toBe('warning');
		expect(strip.querySelector('[data-role="plan-strip-count"]')?.textContent).toBe('0/2');
		expect(strip.querySelector('[data-role="plan-strip-task"]')?.textContent).toBe('заблокирована Ждёт ответа гейта');
	});

	it('nothing_to_decide_draws_nothing', () => {
		tasks = [task('in_progress'), task('waiting'), task('done')];
		expect(show().querySelector('[data-role="plan-strip"]')).toBeNull();
	});
});
