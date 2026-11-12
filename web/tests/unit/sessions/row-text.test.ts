import { describe, expect, it } from 'vitest';
import { catalogTitle, onlineMachines, rowLine } from '#lib/screens/sessions/rowText.js';
import type { ListRow } from '#lib/stores/list.svelte.js';
import type { MachineRecord } from '#lib/stores/machines.svelte.js';

const NOW = Date.parse('2026-11-12T12:00:00.000Z');

function row(patch: Partial<ListRow> = {}): ListRow {
  return {
    id: 'a', title: 'Сессия', title_source: 'user', color: null, pinned: false, archived: false, groups: [], group_pins: [],
    parent_id: null, spawn_depth: 0, machine: { id: 'm', name: 'mini' }, harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    workdir: '/w', created_at: '2026-11-12T10:00:00.000Z', model: 'opus[1m]', effort: 'high', permission_mode: 'owner',
    activity: { state: 'idle', evidence: { last_turn_at: '2026-11-12T11:00:00.000Z' } }, unread_count: 0,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [] }, crunch: { enabled: false }, last_activity_at: null, preview: 'Готово: тесты зелёные',
    ...patch
  };
}

describe('rowLine', () => {
  it('shows the answer preview when the session is calm', () => {
    expect(rowLine(row(), NOW)).toEqual({ who: 'Opus', text: 'Готово: тесты зелёные', tone: 'default', live: false, preview: true });
  });

  it('shows the live status in its tone while something is going on', () => {
    const working = rowLine(row({ activity: { state: 'working', evidence: { phase: 'generating' } } }), NOW);
    expect(working).toMatchObject({ text: 'Генерирует', tone: 'accent', preview: false });
    const waiting = rowLine(row({ activity: { state: 'waiting_for_user', evidence: { question: '2 варианта' } } }), NOW);
    expect(waiting).toMatchObject({ text: 'Ждёт ответа · 2 варианта', tone: 'warning', preview: false });
    const failed = rowLine(row({ activity: { state: 'failed', evidence: { reason: 'process_exited' } } }), NOW);
    expect(failed).toMatchObject({ tone: 'danger', preview: false });
  });

  it('keeps the old status without a preview and for stopped sessions', () => {
    expect(rowLine(row({ preview: null }), NOW)).toMatchObject({ text: 'Ожидает сообщения', preview: false });
    expect(rowLine(row({ activity: { state: 'stopped', evidence: { outcome: 'user_stop' } } }), NOW)).toMatchObject({ text: 'Остановлена вами', preview: false });
  });

  it('names the machine only when several are online', () => {
    expect(rowLine(row(), NOW, { machines: 2 }).who).toBe('Opus · mini');
    const machines = [{ availability: 'ready' }, { availability: 'unavailable' }, { availability: 'degraded' }] as MachineRecord[];
    expect(onlineMachines(machines)).toBe(2);
  });

  it('takes the model title from the machine catalog', () => {
    const machine = { harnesses: [{ id: 'opencode', models: [{ id: 'deepseek/deepseek-v4.1', title: 'DeepSeek V4.1' }] }] } as unknown as MachineRecord;
    const item = row({ harness: { id: 'opencode', title: 'OpenCode', provider: 'opencode' }, model: 'deepseek/deepseek-v4.1' });
    expect(catalogTitle(item, machine)).toBe('DeepSeek V4.1');
    expect(rowLine(item, NOW, { title: catalogTitle(item, machine) }).who).toBe('DeepSeek V4.1');
  });
});
