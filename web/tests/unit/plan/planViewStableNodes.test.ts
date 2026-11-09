import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, tick, unmount } from 'svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { PlanModel } from '#lib/stores/plan.svelte.js';

const plan = new PlanModel();
const items = new SvelteMap<string, unknown>();
vi.mock('#lib/app/runtime.js', () => ({
  getRuntime: () => ({ list: { plan, items, deleted: new SvelteSet() } })
}));
vi.mock('#lib/app/browser.js', () => ({ browserRuntime: () => new Promise(() => undefined) }));

const ORCH = '01900000-0000-7000-8000-00000000000a';
const EXEC = '01900000-0000-7000-8000-00000000000e';
const base = {
  id: '01900000-0000-7000-8000-000000000001',
  orchestrator_id: ORCH,
  number: 1,
  title: 'Хранилище',
  note: 'Заметка',
  blocker: null,
  findings: ['Находка'],
  state: 'in_review' as const,
  executor_id: EXEC,
  attempts: 1,
  created_at: '2026-10-01T00:00:00.000Z',
  updated_at: '2026-10-01T00:00:00.000Z',
  revision: 3,
  commits: [{ sha: 'a1b2c3d4e5', subject: 'Коммит' }],
  commits_truncated: false,
  files_changed: 2,
  report_block_id: '01900000-0000-7000-8000-0000000000bb',
  report_seq: 42
};
let served = [base];
vi.mock('#lib/api/client.js', () => ({
  getPlan: vi.fn(async () => ({ data: { tasks: served.map((task) => structuredClone(task)) } }))
}));

import PlanView from '#lib/screens/plan/PlanView.svelte';

async function settle(): Promise<void> {
  for (let i = 0; i < 4; i++) {
    await Promise.resolve();
    await tick();
    flushSync();
  }
}

describe('PlanView: раскрытая задача держит свои узлы', () => {
  let instance: ReturnType<typeof mount> | undefined;
  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = '';
    plan.clear();
    items.clear();
    served = [base];
    vi.useRealTimers();
  });

  it('ссылка «Отчёт» переживает ответ хаба и живую правку той же задачи', async () => {
    vi.useFakeTimers();
    items.set(EXEC, { id: EXEC, title: 'Исполнитель', model: 'opus', activity: { state: 'idle' } });
    plan.apply(base);
    const target = document.createElement('div');
    document.body.appendChild(target);
    instance = mount(PlanView, { target, props: { orchestratorId: ORCH } });
    await settle();
    target.querySelector<HTMLButtonElement>('[data-plan-task="1"] .head')!.click();
    await settle();
    const report = target.querySelector('[data-role="plan-report"]');
    const body = target.querySelector('[data-role="plan-task-body"]');
    const row = target.querySelector('[data-plan-task="1"]');
    expect(report).not.toBeNull();

    // Правка другой задачи плана: экран перечитывает подробности, ответ хаба несёт ту же задачу.
    plan.apply({ ...base, id: '01900000-0000-7000-8000-000000000002', number: 2, revision: 1 });
    served = [base, { ...base, id: '01900000-0000-7000-8000-000000000002', number: 2, revision: 1 }];
    flushSync();
    await vi.advanceTimersByTimeAsync(500);
    await settle();
    expect(target.querySelector('[data-role="plan-report"]')).toBe(report);
    expect(target.querySelector('[data-role="plan-task-body"]')).toBe(body);
    expect(target.querySelector('[data-plan-task="1"]')).toBe(row);

    // Живая правка самой задачи без смены группы.
    plan.apply({ ...base, note: 'Новая заметка', revision: 4 });
    served = [{ ...base, note: 'Новая заметка', revision: 4 }];
    flushSync();
    await vi.advanceTimersByTimeAsync(500);
    await settle();
    expect(target.textContent).toContain('Новая заметка');
    expect(target.querySelector('[data-role="plan-report"]')).toBe(report);
    expect(target.querySelector('[data-plan-task="1"]')).toBe(row);
  });
});
