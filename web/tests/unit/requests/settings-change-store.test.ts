import { describe, expect, it } from 'vitest';
import { RequestsModel, requestKind } from '#lib/stores/requests.svelte.js';
import {
  waitFactsFromActivity,
  waitFactsFromRequest,
  waitHeadText,
  waitStatusText
} from '#lib/requests/waitCopy.js';

const NOW = Date.parse('2026-11-14T12:00:00.000Z');

const payload = {
  setting: 'routing',
  reason: 'Причина',
  base_revision: 4,
  before: null,
  after: {}
};

function closedEvent(requestId: string, reason: string) {
  return {
    kind: 'request_closed',
    conversation_id: 'conv-1',
    payload: { request_id: requestId, reason }
  } as never;
}

describe('settings_change в хранилище запросов', () => {
  it('распознаётся как отдельный вид запроса', () => {
    expect(requestKind('settings_change')).toBe('settings_change');
    const model = new RequestsModel();
    model.applyOpened({
      kind: 'request_opened',
      conversation_id: 'conv-1',
      committed_at: '2026-11-14T12:00:00.000Z',
      payload: { request_id: 'r1', kind: 'settings_change', payload }
    } as never);
    expect(model.items.get('r1')?.kind).toBe('settings_change');
    expect(model.items.get('r1')?.payload).toEqual(payload);
  });

  it('снимок с финальным состоянием не оставляет карточку открытой', () => {
    const model = new RequestsModel();
    model.ingestSnapshot([
      { id: 'r1', conversation_id: 'conv-1', kind: 'settings_change', state: 'allowed', request_payload: payload },
      { id: 'r2', conversation_id: 'conv-1', kind: 'settings_change', state: 'setting_changed', request_payload: payload },
      { id: 'r3', conversation_id: 'conv-1', kind: 'settings_change', state: 'expired', request_payload: payload },
      { id: 'r4', conversation_id: 'conv-1', kind: 'settings_change', state: 'declined', request_payload: payload }
    ]);
    expect(model.items.get('r1')).toMatchObject({ state: 'applied', closedReason: 'allowed' });
    expect(model.items.get('r2')).toMatchObject({ state: 'closed', closedReason: 'setting_changed' });
    expect(model.items.get('r3')).toMatchObject({ state: 'closed', closedReason: 'expired' });
    expect(model.items.get('r4')).toMatchObject({ state: 'closed', closedReason: 'declined' });
  });

  it('событие request_closed сообщает все исходы хаба', () => {
    const cases: [string, string, string][] = [
      ['allowed', 'applied', 'allowed'],
      ['declined', 'closed', 'declined'],
      ['setting_changed', 'closed', 'setting_changed'],
      ['expired', 'closed', 'expired'],
      ['invalid_value', 'not_applied', 'invalid_value'],
      ['job_failed', 'not_applied', 'job_failed']
    ];
    for (const [reason, state, closedReason] of cases) {
      const model = new RequestsModel();
      model.applyOpened({
        kind: 'request_opened',
        conversation_id: 'conv-1',
        payload: { request_id: 'r1', kind: 'settings_change', payload }
      } as never);
      model.applyDecided({ payload: { request_id: 'r1', decision: { behavior: 'allow' } } } as never);
      model.applyClosed(closedEvent('r1', reason));
      expect(model.items.get('r1'), reason).toMatchObject({ state, closedReason });
    }
  });
});

describe('settings_change в «Ждут вас»', () => {
  it('читается из открытой карточки', () => {
    const facts = waitFactsFromRequest('settings_change', payload, NOW - 12 * 60_000);
    expect(facts.kind).toBe('settings_change');
    expect(waitStatusText(facts, NOW)).toContain('Ждёт решения по настройке');
    expect(waitHeadText(facts, NOW)).toContain('Предложение настройки');
  });

  it('читается из активности списка', () => {
    const facts = waitFactsFromActivity({
      state: 'waiting_for_user',
      evidence: { request_kind: 'settings_change', question: 'Ждёт решения по настройке: routing' },
      since: '2026-11-14T11:48:00.000Z'
    });
    expect(facts?.kind).toBe('settings_change');
    expect(waitStatusText(facts!, NOW)).toContain('Ждёт решения по настройке');
  });
});
