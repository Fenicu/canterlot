import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount } from 'svelte';
import RequestCard from '#lib/requests/RequestCard.svelte';
import { RequestsModel, type RequestItem } from '#lib/stores/requests.svelte.js';
import type { HubEvent } from '#lib/sync/wire.js';

const PAYLOAD = {
  harness: 'codex',
  model: 'gpt-5',
  effort: 'medium',
  title: null,
  workdir: '/work/app',
  isolated: true,
  base: null,
  preset: 'clean',
  permission_mode: 'ask',
  brief: 'Разобрать падающий тест',
  subscribe: true,
  chain_depth: 0
};

beforeEach(() => {
  vi.stubGlobal('matchMedia', (query: string) => ({
    media: query,
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    onchange: null,
    dispatchEvent: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn()
  }));
});

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.textContent = '';
});

function render(request: RequestItem, onDecision = vi.fn(async () => undefined)) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  mount(RequestCard, { target, props: { request, onDecision } });
  flushSync();
  return { target, onDecision };
}

describe('spawn approval request', () => {
  it('spawn_approval_is_not_a_question', () => {
    const model = new RequestsModel('conv-1');
    model.ingestSnapshot([{ id: 'r1', conversation_id: 'conv-1', kind: 'spawn_approval', state: 'open', request_payload: PAYLOAD }]);
    model.ingestHistory({ id: 'r2', request_kind: 'spawn_approval', state: 'decision_applied', request_payload: PAYLOAD });
    model.applyOpened({
      kind: 'request_opened',
      conversation_id: 'conv-1',
      payload: { request_id: 'r3', kind: 'spawn_approval', payload: PAYLOAD }
    } as unknown as HubEvent);
    expect(model.items.get('r1')?.kind).toBe('spawn_approval');
    expect(model.items.get('r2')?.kind).toBe('spawn_approval');
    expect(model.items.get('r3')?.kind).toBe('spawn_approval');

    const { target } = render(model.items.get('r1')!);
    expect(target.textContent).toContain('Агент просит запустить сессию');
    expect(target.textContent).not.toContain('Вопрос от агента');
    for (const word of ['Харнесс', 'Модель', 'Режим', 'Каталог', 'Отдельная рабочая копия', 'Задача']) {
      expect(target.textContent).toContain(word);
    }
    expect(target.textContent).toContain('/work/app');
    expect(target.textContent).toContain('Разобрать падающий тест');
  });

  it('unknown_kinds_still_fall_back_to_a_question', () => {
    const model = new RequestsModel('conv-1');
    model.ingestSnapshot([{ id: 'r1', conversation_id: 'conv-1', kind: 'something_new', state: 'open', request_payload: {} }]);
    expect(model.items.get('r1')?.kind).toBe('question');
  });

  it('allow_and_deny_send_the_permission_shape', () => {
    const model = new RequestsModel('conv-1');
    model.ingestSnapshot([{ id: 'r1', conversation_id: 'conv-1', kind: 'spawn_approval', state: 'open', request_payload: PAYLOAD }]);
    const { target, onDecision } = render(model.items.get('r1')!);
    target.querySelector<HTMLButtonElement>('[data-action="allow"]')!.click();
    target.querySelector<HTMLButtonElement>('[data-action="deny"]')!.click();
    expect(onDecision).toHaveBeenNthCalledWith(1, { kind: 'permission', behavior: 'allow' });
    expect(onDecision).toHaveBeenNthCalledWith(2, { kind: 'permission', behavior: 'deny' });
  });

  it('a_decided_card_collapses_to_a_step_with_the_outcome', () => {
    const model = new RequestsModel('conv-1');
    model.ingestSnapshot([
      { id: 'r1', conversation_id: 'conv-1', kind: 'spawn_approval', state: 'decision_applied', request_payload: PAYLOAD, decision_payload: { kind: 'permission', behavior: 'deny' } }
    ]);
    const { target } = render(model.items.get('r1')!);
    expect(target.textContent).toContain('Запуск сессии');
    expect(target.textContent).toContain('Отклонено');
  });
});
