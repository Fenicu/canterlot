import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';
import { alarmChip, alarmDetail, alarmEventLine, alarmSummary } from '#lib/timeline/alarm.js';
import type { AlarmView } from '#lib/stores/chat.svelte.js';

const base: AlarmView = {
  alarm_id: '00000000-0000-4000-8000-0000000000a1',
  conversation_id: '00000000-0000-4000-8000-0000000000c1',
  key: 'gate',
  kind: 'at',
  condition: 'at 2026-11-04 14:30 UTC',
  session_ids: [],
  states: [],
  fires_at: '2026-11-04T14:30:00.000Z',
  deadline_at: null,
  only_on_timeout: false,
  batch: false,
  state: 'armed',
  fired_reason: null,
  failure_reason: null,
  fired_at: null,
  note: null,
  note_bytes: 0,
  created_at: '2026-11-04T12:30:00.000Z'
};

describe('alarm plaques', () => {
  it('names the time of a timed alarm', () => {
    const chip = alarmChip(base);
    expect(chip).toContain('Будильник');
    expect(chip).toMatch(/\d{2}:\d{2}/);
  });

  it('names the session condition and the deadline', () => {
    const chip = alarmChip({
      ...base,
      kind: 'condition',
      fires_at: null,
      deadline_at: '2026-11-04T18:00:00.000Z',
      session_ids: ['00000000-0000-4000-8000-0000000000e5'],
      states: ['idle', 'failed']
    });
    expect(chip).toContain('когда');
    expect(chip).toContain('00000000');
    expect(chip).toContain('закончится или сорвётся');
    expect(chip).toContain('до');
  });

  it('shows a limit-reset condition in Russian from structured fields', () => {
    const alarm = {
      ...base,
      kind: 'condition',
      fires_at: null,
      deadline_at: '2026-11-05T12:00:00.000Z',
      condition: 'claude five_hour window resets after 2026-11-05 10:00 UTC; deadline 2026-11-05 12:00 UTC',
      provider: 'claude',
      window: 'five_hour',
      below_percent: 40
    };
    expect(alarmChip(alarm)).toBe(
      'Будильник · «gate» · Сброс лимита Claude, окно 5 ч или расход не выше 40%'
    );
    expect(alarmDetail(alarm)).toContain(
      'Условие: Сброс лимита Claude, окно 5 ч или расход не выше 40%'
    );
  });

  it('shows a machine condition and its thresholds in Russian', () => {
    const alarm = {
      ...base,
      kind: 'condition',
      fires_at: null,
      deadline_at: '2026-11-05T12:00:00.000Z',
      condition: 'machine: free disk above 50 GB, load below 0.5 per core; deadline 2026-11-05 12:00 UTC',
      free_disk_gb_above: 50,
      load_per_core_below: 0.5,
      mem_available_gb_above: 8
    };
    const text =
      'Машина: свободно на диске больше 50 ГБ, нагрузка на ядро ниже 0,5, память больше 8 ГБ';
    expect(alarmChip(alarm)).toBe(`Будильник · «gate» · ${text}`);
    expect(alarmDetail(alarm)).toContain(`Условие: ${text}`);
  });

  it('names a limit-reset watchdog in Russian', () => {
    const chip = alarmChip({
      ...base,
      kind: 'condition',
      only_on_timeout: true,
      deadline_at: '2026-11-05T12:00:00.000Z',
      condition: 'claude weekly window resets; deadline 2026-11-05 12:00 UTC',
      provider: 'claude',
      window: 'weekly'
    });
    expect(chip).toContain('Сторож · Сброс лимита Claude, окно неделя · до');
  });

  it('marks a watchdog and a failed delivery', () => {
    expect(alarmChip({ ...base, only_on_timeout: true })).toContain('Сторож');
    expect(alarmChip({ ...base, state: 'failed', failure_reason: 'delivery_failed' })).toContain(
      'не доставлен'
    );
    expect(
      alarmDetail({ ...base, state: 'failed', failure_reason: 'delivery_failed' })
    ).toContain('Не удалось доставить сообщение');
  });

  it('builds the quiet feed line from structured facts', () => {
    expect(alarmEventLine({ alarm_key: 'gate', fired_reason: 'time' })).toBe(
      'Будильник «gate»: сработал'
    );
    expect(alarmEventLine({ alarm_key: 'gate', fired_reason: 'timed_out' })).toContain(
      'истёк срок'
    );
    expect(alarmEventLine({ alarm_key: 'gate', fired_reason: 'by_clock' })).toBe(
      'Будильник «gate»: лимит, вероятно, сброшен (по часам, без подтверждения)'
    );
  });

  it('summarises several alarms with the nearest moment and Russian word forms', () => {
    const second = {
      ...base,
      alarm_id: '00000000-0000-4000-8000-0000000000a2',
      fires_at: '2026-11-04T14:00:00.000Z'
    };
    const watchdog = {
      ...base,
      alarm_id: '00000000-0000-4000-8000-0000000000a3',
      kind: 'condition' as const,
      only_on_timeout: true,
      fires_at: null,
      deadline_at: '2026-11-04T15:00:00.000Z'
    };
    expect(alarmSummary([base, second, watchdog])).toBe(
      `Будильники · 3 · ближайший ${fmt.clock(Date.parse(second.fires_at!))}`
    );
    expect(alarmSummary([base])).toMatch(/^Будильник · 1 · ближайший \d{2}:\d{2}$/);
    expect(alarmSummary([{ ...base, fires_at: null }])).toBe('Будильник · 1');
  });
});
