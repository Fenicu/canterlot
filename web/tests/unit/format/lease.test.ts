import { describe, expect, it } from 'vitest';
import { leaseKindLabel, leaseRows, leaseWaitOf, leaseWaitText, type LeaseEntry } from '#lib/format/lease.js';

const NOW = Date.parse('2026-11-08T12:00:00.000Z');

function entry(patch: Partial<LeaseEntry>): LeaseEntry {
  return {
    lease_id: 'l', kind: 'build', weight: 1, label: '', holder: 'execution', execution_id: null,
    conversation_id: null, since: null, requested_at: null, position: null, dir: '', ...patch
  };
}

describe('lease text', () => {
  it('names the queue and the ordinal place', () => {
    expect(leaseWaitText({ kind: 'build', position: 2 })).toBe('Ждёт очереди на сборку · 2-й');
    expect(leaseWaitText({ kind: 'gate', position: 1 })).toBe('Ждёт очереди на гейт · 1-й');
    expect(leaseWaitText({ kind: 'build', position: 0 })).toBe('Ждёт очереди на сборку · 1-й');
    expect(leaseKindLabel('gate')).toBe('гейт');
    expect(leaseKindLabel('build')).toBe('сборка');
  });

  it('reads the wait only from working evidence', () => {
    const wait = { kind: 'gate', position: 3, since: '2026-11-08T11:00:00.000Z' };
    expect(leaseWaitOf({ state: 'working', evidence: { phase: 'running_tool', lease_wait: wait } })).toEqual({ kind: 'gate', position: 3 });
    expect(leaseWaitOf({ state: 'stalled', evidence: { lease_wait: wait } })).toBeNull();
    expect(leaseWaitOf({ state: 'working', evidence: { phase: 'running_tool' } })).toBeNull();
    expect(leaseWaitOf({ state: 'working', evidence: { lease_wait: { kind: 'gate' } } })).toBeNull();
    expect(leaseWaitOf(null)).toBeNull();
  });

  it('lists holders with titles and durations and waiters by place', () => {
    const titles: Record<string, string> = { c1: 'Баннер связи' };
    const rows = leaseRows({
      slots: 2,
      observed_at: '2026-11-08T12:00:00.000Z',
      holders: [
        entry({ lease_id: 'a', conversation_id: 'c1', since: '2026-11-08T11:48:00.000Z' }),
        entry({ lease_id: 'b', holder: 'owner', since: '2026-11-08T10:30:00.000Z' })
      ],
      waiters: [
        entry({ lease_id: 'w2', kind: 'build', label: 'check.sh', position: 2 }),
        entry({ lease_id: 'w1', kind: 'gate', conversation_id: 'gone', dir: 'canterlot-2a', position: 1 })
      ]
    }, (id) => titles[id], NOW);
    expect(rows.holders.map((r) => [r.name, r.kind, r.detail])).toEqual([
      ['Баннер связи', 'сборка', '12 мин'],
      ['вы', 'сборка', '1 ч 30 мин']
    ]);
    expect(rows.waiters.map((r) => [r.name, r.kind, r.detail])).toEqual([
      ['canterlot-2a', 'гейт', '1-й'],
      ['check.sh', 'сборка', '2-й']
    ]);
    expect(rows.holders[0]!.conversationId).toBe('c1');
    expect(rows.holders[1]!.conversationId).toBeNull();
  });

  it('gives empty lists without a queue', () => {
    expect(leaseRows(null, () => undefined, NOW)).toEqual({ holders: [], waiters: [] });
  });
});
