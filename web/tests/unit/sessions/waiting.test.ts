import { describe, expect, it } from 'vitest';
import { buildSections } from '#lib/screens/sessions/grouping.js';
import { otherWaitingCount, waitsForOwner, waitingRows } from '#lib/screens/sessions/waiting.js';
import type { ListRow } from '#lib/stores/list.svelte.js';

function row(patch: Partial<ListRow> & { id: string; title: string }): ListRow {
  return {
    title_source: 'user',
    color: null,
    pinned: false,
    archived: false,
    groups: [],
    group_pins: [],
    parent_id: null,
    spawn_depth: 0,
    machine: { id: 'm', name: 'workstation' },
    harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
    workdir: '/work',
    created_at: '2026-10-28T12:00:00.000Z',
    model: 'opus',
    effort: 'medium',
    permission_mode: 'default',
    activity: { state: 'idle' },
    unread_count: 0,
    queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
    crunch: { enabled: false },
    last_activity_at: null,
    ...patch
  };
}

function waitingAt(at: string, evidence: Record<string, unknown> = { phase: 'awaiting_permission', question: 'Bash: cargo test' }) {
  return { state: 'waiting_for_user' as const, evidence, since: at };
}

const groups = [
  { id: 'canterlot', name: 'canterlot', position: 0 },
  { id: 'review', name: 'На проверку', position: 1 }
];

describe('sessions/waiting', () => {
  it('counts_permission_question_link_and_stalled_as_waiting', () => {
    const items = [
      row({ id: 'p', title: 'Разрешение', activity: waitingAt('2026-10-28T12:00:00.000Z') }),
      row({ id: 'q', title: 'Вопрос', activity: waitingAt('2026-10-28T12:01:00.000Z', { phase: 'awaiting_answer', question: 'Какой порог?' }) }),
      row({ id: 'l', title: 'Ссылка', activity: waitingAt('2026-10-28T12:02:00.000Z', { request_kind: 'elicitation' }) }),
      row({ id: 's', title: 'Подвисла', activity: { state: 'stalled', evidence: { last_event_at: '2026-10-28T12:03:00.000Z' }, since: '2026-10-28T11:00:00.000Z' } })
    ];
    expect(items.map(waitsForOwner)).toEqual([true, true, true, true]);
  });

  it('leaves_running_and_idle_sessions_out', () => {
    const items = [
      row({ id: 'run', title: 'Генерирует', activity: { state: 'working', evidence: { phase: 'generating' }, since: '2026-10-28T12:00:00.000Z' } }),
      row({ id: 'tool', title: 'Инструмент', activity: { state: 'working', evidence: { phase: 'running_tool' }, since: '2026-10-28T12:00:00.000Z' } }),
      row({ id: 'idle', title: 'Покой', activity: { state: 'idle', evidence: { last_turn_at: '2026-10-28T12:00:00.000Z' } } }),
      row({ id: 'wait', title: 'Ждёт', activity: waitingAt('2026-10-28T12:00:00.000Z') })
    ];
    expect(items.map(waitsForOwner)).toEqual([false, false, false, true]);
    expect(waitingRows(items, { kind: 'all' }).map((item) => item.id)).toEqual(['wait']);
  });

  it('sorts_the_oldest_wait_first', () => {
    const items = [
      row({ id: 'young', title: 'Свежая', activity: waitingAt('2026-10-28T12:30:00.000Z') }),
      row({ id: 'old', title: 'Долгая', activity: waitingAt('2026-10-28T10:00:00.000Z') }),
      row({ id: 'mid', title: 'Средняя', activity: { state: 'stalled', evidence: { last_event_at: '2026-10-28T11:00:00.000Z' }, since: '2026-10-28T09:00:00.000Z' } })
    ];
    expect(waitingRows(items, { kind: 'all' }).map((item) => item.id)).toEqual(['old', 'mid', 'young']);
  });

  it('counts_other_sessions_only_and_skips_archived_and_trashed', () => {
    const items = [
      row({ id: 'me', title: 'Текущая', activity: waitingAt('2026-10-28T12:00:00.000Z') }),
      row({ id: 'b', title: 'Другая', activity: waitingAt('2026-10-28T12:00:00.000Z') }),
      row({ id: 'c', title: 'Третья', activity: { state: 'stalled', evidence: {}, since: '2026-10-28T12:00:00.000Z' } }),
      row({ id: 'arch', title: 'Архив', archived: true, activity: waitingAt('2026-10-28T12:00:00.000Z') }),
      row({ id: 'trash', title: 'Корзина', trashed_at: '2026-10-28T12:00:00.000Z', activity: waitingAt('2026-10-28T12:00:00.000Z') }),
      row({ id: 'run', title: 'Работает', activity: { state: 'working', evidence: { phase: 'generating' }, since: '2026-10-28T12:00:00.000Z' } })
    ];
    expect(otherWaitingCount(items, 'me')).toBe(2);
    expect(otherWaitingCount(items, '')).toBe(3);
  });

  it('puts_the_waiting_section_first_and_keeps_the_same_rows_in_their_groups', () => {
    const items = [
      row({ id: 'pinned', title: 'Закреп', pinned: true, groups: ['canterlot'] }),
      row({ id: 'can', title: 'В группе', groups: ['canterlot'], activity: waitingAt('2026-10-28T12:20:00.000Z') }),
      row({ id: 'rev', title: 'Во второй', groups: ['review'], activity: waitingAt('2026-10-28T12:10:00.000Z') })
    ];
    const sections = buildSections(items, groups, { kind: 'all' }, new Set());
    expect(sections[0]?.id).toBe('waiting');
    expect(sections[0]?.title).toBe('Ждут вас');
    expect(sections[0]?.count).toBe(2);
    expect(sections[0]?.items.map((node) => node.item.id)).toEqual(['rev', 'can']);
    // Копии, а не перенос: обе строки остаются в своих группах.
    expect(sections.find((section) => section.id === 'pinned')?.items.map((node) => node.item.id)).toEqual(['pinned']);
    expect(sections.find((section) => section.id === 'canterlot')?.items.map((node) => node.item.id)).toEqual(['can']);
    expect(sections.find((section) => section.id === 'review')?.items.map((node) => node.item.id)).toEqual(['rev']);
  });

  it('shows_only_the_group_waiters_in_a_group_tab_and_none_in_the_pinned_tab', () => {
    const items = [
      row({ id: 'can', title: 'В группе', groups: ['canterlot'], activity: waitingAt('2026-10-28T12:20:00.000Z') }),
      row({ id: 'rev', title: 'Во второй', groups: ['review'], pinned: true, activity: waitingAt('2026-10-28T12:10:00.000Z') })
    ];
    const group = buildSections(items, groups, { kind: 'group', id: 'canterlot' }, new Set());
    expect(group[0]?.id).toBe('waiting');
    expect(group[0]?.items.map((node) => node.item.id)).toEqual(['can']);
    const pinned = buildSections(items, groups, { kind: 'pinned' }, new Set());
    expect(pinned.some((section) => section.id === 'waiting')).toBe(false);
  });

  it('has_no_section_without_waiters', () => {
    const items = [row({ id: 'run', title: 'Работает', activity: { state: 'working', evidence: { phase: 'generating' }, since: '2026-10-28T12:00:00.000Z' } })];
    const sections = buildSections(items, groups, { kind: 'all' }, new Set());
    expect(sections.some((section) => section.id === 'waiting')).toBe(false);
  });
});
