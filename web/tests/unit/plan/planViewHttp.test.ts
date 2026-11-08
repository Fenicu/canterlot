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
vi.mock('#lib/api/client.js', () => ({
  getPlan: vi.fn(async () => ({ data: { tasks: [task] } }))
}));

import PlanView from '#lib/screens/plan/PlanView.svelte';
import { getPlan } from '#lib/api/client.js';

describe('PlanView: план вне снимка списка', () => {
  let instance: ReturnType<typeof mount> | undefined;
  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = '';
  });

  // Снимок владельца не несёт планы архивных оркестраторов, делегатов и готовые задачи старше
  // 7 дней; GET /api/v1/plan (для глубоких ссылок) отдаёт их, но экран рисует только снимок.
  it.skip('bug: глубокая ссылка /plan/<id> на план вне снимка показывает пустой план, хотя хаб вернул задачи', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    instance = mount(PlanView, { target, props: { orchestratorId: ORCH } });
    flushSync();
    await Promise.resolve();
    await Promise.resolve();
    await tick();
    flushSync();
    expect(getPlan).toHaveBeenCalledWith(ORCH);
    expect(target.textContent).not.toContain('В плане пока нет задач');
    expect(target.querySelector('[data-plan-group="done"]')).not.toBeNull();
  });
});
