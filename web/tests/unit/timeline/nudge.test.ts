import { describe, expect, test } from 'vitest';
import {
  nudgeCounter,
  nudgeEntryLine,
  nudgeExhaustedLine,
  nudgeStateText,
  signalLabels,
} from '#lib/timeline/nudge.js';

const rule = (patch: Record<string, unknown> = {}) => ({
  session_id: 'c',
  enabled: true,
  state: 'watching',
  idle_minutes: 3,
  background_minutes: 10,
  max_consecutive: 3,
  max_total: 20,
  done_markers: ['STATUS: DONE'],
  context_warn_percent: 80,
  set_by: 'agent',
  set_by_session: null,
  notify_session_id: null,
  consecutive: 0,
  total: 0,
  held: false,
  ...patch,
});

describe('nudge state text', () => {
  test('names each state in Russian', () => {
    expect(nudgeStateText(rule() as never)).toBe('Автопинок: следит');
    expect(nudgeStateText(rule({ state: 'pending', idle_minutes: 6 }) as never)).toBe(
      'Автопинок: ждёт толчка через 6 мин',
    );
    expect(nudgeStateText(rule({ state: 'exhausted' }) as never)).toBe('Автопинок: исчерпан');
    expect(nudgeStateText(rule({ state: 'paused' }) as never)).toBe('Автопинок: на паузе');
    expect(nudgeStateText(rule({ enabled: false }) as never)).toBe('Автопинок: на паузе');
  });

  test('counts sent nudges against the total limit', () => {
    expect(nudgeCounter(rule({ total: 2, max_total: 20 }) as never)).toBe('толчков 2 из 20');
    expect(nudgeCounter(rule({ total: 0, max_total: 20 }) as never)).toBe('толчков 0 из 20');
  });
});

describe('nudge signal labels', () => {
  test('translates structured signals and keeps their order', () => {
    expect(
      signalLabels({
        no_marker: true,
        continue_phrase: true,
        uncommitted: true,
        background_suspected: true,
        short_turn: true,
        context_high: true,
        question_in_text: true,
      }),
    ).toEqual([
      'нет маркера готовности',
      'обещал продолжить',
      'незакоммиченные изменения',
      'фоновая работа',
      'короткий ход',
      'контекст заполнен',
      'спросил текстом',
    ]);
  });

  test('ignores unknown and false fields', () => {
    expect(signalLabels({ no_marker: true, unknown: true, uncommitted: false })).toEqual([
      'нет маркера готовности',
    ]);
    expect(signalLabels(null)).toEqual([]);
  });

  test('names the report file signals in Russian', () => {
    expect(signalLabels({ report_missing: true, report_probe_failed: true })).toEqual([
      'отчёт ещё не появился',
      'отчёт не удалось проверить',
    ]);
  });
});

describe('nudge feed line', () => {
  test('shows the attempt and the first signals', () => {
    expect(
      nudgeEntryLine({
        kind: 'nudge',
        chain_depth: 0,
        attempt: 2,
        max: 3,
        signals: { no_marker: true, continue_phrase: true },
      } as never),
    ).toBe('Автопинок: толчок 2 из 3 · нет маркера готовности, обещал продолжить');
  });

  test('falls back to the delivered text when the attempt is unknown', () => {
    expect(
      nudgeEntryLine({ kind: 'nudge', chain_depth: 0 } as never),
    ).toBe('Автопинок: толчок');
  });

  test('states exhaustion with the number of nudges', () => {
    expect(nudgeExhaustedLine({ kind: 'session_result', nudges: 3, nudge_exhausted: true } as never)).toBe(
      'Автопинок сдался после 3 толчков без прогресса',
    );
    expect(nudgeExhaustedLine({ kind: 'session_result' } as never)).toBeNull();
  });
});
