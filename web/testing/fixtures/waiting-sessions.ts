import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation, seedMachine } from '../fake-hub/seed';

const AT = '2026-10-28T12:00:00.000Z';
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

export interface WaitingSessions {
  groups: { work: string; home: string };
  ids: { current: string; older: string; newer: string; running: string };
}

/** Стол ожидания: одна сессия работает, две ждут владельца в разных группах. */
export function seedWaitingSessions(api: FakeHubApi): WaitingSessions {
  const machine = seedMachine(api, { id: api.id(), name: 'workstation', availability: 'ready' });
  const work = api.id();
  const home = api.id();
  api.groups = [
    { id: work, name: 'Работа', position: 0, created_at: AT },
    { id: home, name: 'Дом', position: 1, created_at: AT }
  ];
  const common = {
    machine: { id: machine.id, name: 'workstation' },
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    model: 'opus-5.5'
  };
  const current = seedConversation(api, {
    ...common,
    title: 'Текущая сессия',
    groups: [work],
    activity: { state: 'idle', evidence: { last_turn_at: ago(3) }, since: ago(3) }
  });
  const older = seedConversation(api, {
    ...common,
    title: 'Долгий вопрос',
    groups: [home],
    activity: { state: 'waiting_for_user', evidence: { phase: 'awaiting_answer', question: 'Какой порог?' }, since: ago(42) }
  });
  const newer = seedConversation(api, {
    ...common,
    title: 'Свежее разрешение',
    groups: [work],
    activity: { state: 'waiting_for_user', evidence: { phase: 'awaiting_permission', question: 'Bash: cargo test' }, since: ago(7) }
  });
  const running = seedConversation(api, {
    ...common,
    title: 'Сборка идёт',
    groups: [work],
    activity: { state: 'working', evidence: { phase: 'generating' }, since: ago(2) }
  });
  api.counts = {
    all: 4,
    pinned: 0,
    archived: 0,
    trashed: 0,
    groups: [
      { id: work, count: 3 },
      { id: home, count: 1 }
    ]
  };
  return { groups: { work, home }, ids: { current, older, newer, running } };
}
