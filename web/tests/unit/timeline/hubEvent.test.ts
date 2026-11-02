import { expect, test } from 'vitest';
import {
  LEGACY_LIMIT,
  collapsedText,
  eventFacts,
  eventLine,
  eventSessionId,
  eventTitle,
  isLongText,
  isSessionEvent,
  sessionStatus
} from '#lib/timeline/hubEvent.js';

test('session_status_is_a_russian_word', () => {
  expect(sessionStatus({ kind: 'session_result', state: 'idle' })).toBe('завершена');
  expect(sessionStatus({ kind: 'asked', state: 'waiting_for_user' })).toBe('задала вопрос');
  expect(sessionStatus({ kind: 'stalled', state: 'stalled' })).toBe('зависла');
  expect(sessionStatus({ kind: 'failed', state: 'failed' })).toBe('сорвалась');
  expect(sessionStatus({ kind: 'quota_paused', state: 'quota_paused' })).toBe('ждёт лимита');
  expect(sessionStatus({ kind: 'session_result' })).toBe('завершена');
});

test('session_kinds_get_a_card', () => {
  for (const kind of ['session_result', 'asked', 'stalled', 'failed', 'quota_paused'])
    expect(isSessionEvent({ kind })).toBe(true);
  for (const kind of ['accepted', 'stopped', 'approval', 'deleted'])
    expect(isSessionEvent({ kind })).toBe(false);
});

test('session_facts_use_the_owner_language', () => {
  expect(
    eventFacts({
      kind: 'session_result',
      turns: 4,
      duration_ms: 372_000,
      tokens_in: 12_000,
      tokens_out: 900,
      commits: [
        { sha: '5d6e7f8', title: 'Parse the header' },
        { sha: '9a0b1c2', title: 'Add tests' }
      ],
      changed_files: 3
    })
  ).toEqual([
    { label: 'ходы', value: '4' },
    { label: 'время', value: '6 мин 12 с' },
    { label: 'коммиты', value: '2' },
    { label: 'файлы', value: '3' }
  ]);
});

test('session_facts_skip_what_the_hub_does_not_know', () => {
  expect(eventFacts({ kind: 'session_result', turns: 0, duration_ms: 0 })).toEqual([
    { label: 'ходы', value: '0' },
    { label: 'время', value: '0,0 с' }
  ]);
});

test('accepted_line_names_the_fate_of_the_worktree', () => {
  expect(
    eventLine(
      {
        kind: 'accepted',
        worktree: 'removed',
        branch: 'canterlot/0190abcd',
        branch_kept: true
      },
      'Карантин Rust-тестов'
    )
  ).toBe('«Карантин Rust-тестов» принята, рабочая копия убрана, ветка canterlot/0190abcd оставлена');
  expect(
    eventLine(
      { kind: 'accepted', worktree: 'removed', branch: 'canterlot/0190abcd', branch_kept: false },
      'Карантин Rust-тестов'
    )
  ).toBe('«Карантин Rust-тестов» принята, рабочая копия убрана, ветка canterlot/0190abcd удалена');
  expect(eventLine({ kind: 'accepted', worktree: 'removed', branch_kept: true }, 'Сессия')).toBe(
    '«Сессия» принята, рабочая копия убрана'
  );
  expect(eventLine({ kind: 'accepted', worktree: 'none' }, 'Сессия')).toBe(
    '«Сессия» принята, сессия остановлена и в архиве'
  );
  expect(eventLine({ kind: 'accepted', worktree: 'dirty' }, 'Сессия')).toBe(
    '«Сессия» остановлена, в рабочей копии остались правки'
  );
  expect(eventLine({ kind: 'accepted', worktree: 'failed' }, 'Сессия')).toBe(
    '«Сессия» остановлена, рабочую копию убрать не удалось'
  );
  expect(eventLine({ kind: 'accepted', closed: true }, 'Сессия')).toBe(
    '«Сессия» принята родителем, события больше не придут'
  );
});

test('approval_line_names_the_outcome', () => {
  expect(eventLine({ kind: 'approval', decision: 'allowed' }, 'Миграция схемы')).toBe(
    'Запуск «Миграция схемы» разрешён'
  );
  expect(eventLine({ kind: 'approval', decision: 'denied' }, 'Миграция схемы')).toBe(
    'Запуск сессии отклонён владельцем'
  );
  expect(eventLine({ kind: 'approval', decision: 'failed' }, 'Миграция схемы')).toBe(
    'Запуск разрешён, но сессия не стартовала'
  );
  expect(eventLine({ kind: 'approval', decision: 'lapsed' }, 'Миграция схемы')).toBe(
    'Запрос на запуск истёк'
  );
});

test('stopped_and_deleted_lines_are_quiet', () => {
  expect(eventLine({ kind: 'stopped' }, 'Сборка')).toBe('«Сборка» остановлена');
  expect(eventLine({ kind: 'deleted' }, 'Сборка')).toBe('«Сборка» удалена');
  expect(eventLine({ kind: 'deleted' }, '')).toBe('Сессия удалена');
});

test('title_and_session_link_fall_back_to_the_origin', () => {
  expect(eventTitle({ kind: 'event', title: ' Сборка ', chain_depth: 1 })).toBe('Сборка');
  expect(eventTitle({ kind: 'event', title: null, chain_depth: 1 })).toBe('Сессия');
  expect(
    eventSessionId({
      kind: 'event',
      from: '00000000-0000-4000-8000-000000000001',
      chain_depth: 1,
      event: { kind: 'session_result', session_id: '00000000-0000-4000-8000-000000000002' }
    })
  ).toBe('00000000-0000-4000-8000-000000000002');
  expect(
    eventSessionId({ kind: 'event', from: '00000000-0000-4000-8000-000000000001', chain_depth: 1 })
  ).toBe('00000000-0000-4000-8000-000000000001');
  expect(eventSessionId({ kind: 'event', chain_depth: 1 })).toBeNull();
});

test('old_rows_collapse_only_long_text', () => {
  expect(isLongText('x'.repeat(LEGACY_LIMIT))).toBe(false);
  expect(isLongText('x'.repeat(LEGACY_LIMIT + 1))).toBe(true);
  expect(collapsedText('x'.repeat(LEGACY_LIMIT + 20))).toBe(`${'x'.repeat(LEGACY_LIMIT)}…`);
  expect(collapsedText('short')).toBe('short');
});
