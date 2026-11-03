import { describe, expect, it } from 'vitest';
import { alarmChip, alarmDetail, alarmEventLine } from '#lib/timeline/alarm.js';
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
  });
});
