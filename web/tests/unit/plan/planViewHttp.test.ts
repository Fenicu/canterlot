import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, tick, unmount } from 'svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { PlanModel } from '#lib/stores/plan.svelte.js';

const plan = new PlanModel();
vi.mock('#lib/app/runtime.js', () => ({
  getRuntime: () => ({ list: { plan, items: new SvelteMap(), deleted: new SvelteSet() } })
}));
vi.mock('#lib/app/browser.js', () => ({ browserRuntime: () => new Promise(() => undefined) }));

const ORCH = '01900000-0000-7000-8000-00000000000a';
const task = {
  id: '01900000-0000-7000-8000-000000000001',
  orchestrator_id: ORCH,
  number: 1,
  title: 'Миграция',
  note: null,
  blocker: null,
  findings: [],
  state: 'done',
  executor_id: null,
  attempts: 0,
  created_at: '2026-10-01T00:00:00.000Z',
  updated_at: '2026-10-01T00:00:00.000Z',
  revision: 3,
  commits: [],
  commits_truncated: false,
  files_changed: 0,
  report_block_id: null,
  report_seq: null
};
let served: (typeof task)[] = [task];
vi.mock('#lib/api/client.js', () => ({
  getPlan: vi.fn(async () => ({ data: { tasks: served } }))
}));

import PlanView from '#lib/screens/plan/PlanView.svelte';
import { getPlan } from '#lib/api/client.js';

describe('PlanView: план вне снимка списка', () => {
  let instance: ReturnType<typeof mount> | undefined;
  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = '';
    plan.clear();
    served = [task];
  });

  async function render(id = ORCH): Promise<HTMLElement> {
    const target = document.createElement('div');
    document.body.appendChild(target);
    instance = mount(PlanView, { target, props: { orchestratorId: id } });
    flushSync();
    await Promise.resolve();
    await Promise.resolve();
    await tick();
    flushSync();
    return target;
  }

  // Снимок владельца не несёт планы архивных оркестраторов, делегатов и готовые задачи старше
  // 7 дней; GET /api/v1/plan (для глубоких ссылок) отдаёт их, но экран рисует только снимок.
  it('глубокая ссылка /plan/<id> на архивный план вне снимка показывает задачи из ответа хаба', async () => {
    const target = await render();
    expect(getPlan).toHaveBeenCalledWith(ORCH);
    expect(target.textContent).not.toContain('В плане пока нет задач');
    expect(target.querySelector('[data-plan-group="done"]')).not.toBeNull();
  });

  it('план делегата вне снимка: задачи из ответа хаба, без чужих задач снимка', async () => {
    const DELEGATE = '01900000-0000-7000-8000-00000000000d';
    served = [{ ...task, id: '01900000-0000-7000-8000-000000000002', orchestrator_id: DELEGATE, title: 'Шаг делегата', state: 'in_progress' }];
    plan.apply({ ...task, id: '01900000-0000-7000-8000-000000000003', title: 'Чужая задача', state: 'waiting' });
    const target = await render(DELEGATE);
    expect(getPlan).toHaveBeenCalledWith(DELEGATE);
    expect(target.textContent).toContain('Шаг делегата');
    expect(target.textContent).not.toContain('Чужая задача');
  });

  it('старые готовые задачи из ответа хаба стоят рядом с живыми задачами снимка', async () => {
    const live = { ...task, id: '01900000-0000-7000-8000-000000000004', number: 2, title: 'Живая', state: 'in_progress' as const, revision: 1 };
    plan.apply(live);
    served = [task, { ...live, revision: 1 }];
    const target = await render();
    expect(target.querySelector('[data-plan-group="done"]')).not.toBeNull();
    expect(target.textContent).toContain('Живая');
    // Живая правка новее ответа хаба побеждает.
    plan.apply({ ...live, state: 'cancelled', revision: 2 });
    flushSync();
    expect(target.querySelector('[data-plan-group="cancelled"]')).not.toBeNull();
    expect(target.querySelector('[data-plan-group="in_progress"]')).toBeNull();
  });

  it('удалённый оркестратор: задачи из ответа хаба уходят вместе с ним', async () => {
    const target = await render();
    expect(target.querySelector('[data-plan-group="done"]')).not.toBeNull();
    plan.dropOrchestrator(ORCH);
    flushSync();
    expect(target.querySelector('[data-plan-task]')).toBeNull();
  });
});
