import type { HistoryInput } from '../../src/lib/stores/chat.svelte';
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';

const AT = '2026-11-10T12:00:00.000Z';
const EARLIER = '2026-11-10T10:21:00.000Z';
const RESET = '2026-11-10T04:00:00.000Z';

const AUTOMATION = 'This is canterlot automation, not the owner.';

export type SystemBubbleGroup = 'alarm' | 'machine' | 'context' | 'limit' | 'message' | 'all';

type Seed = { child: string; ownerInput: string };

function input(
  api: FakeHubApi,
  feed: number,
  text: string,
  origin: Record<string, unknown>,
  createdAt = AT
): HistoryInput {
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `system-${feed}`,
    state: 'accepted',
    content: [{ type: 'text', text }],
    attachments: [],
    created_seq: String(feed),
    feed_seq: String(feed),
    created_at: createdAt,
    origin
  } as HistoryInput;
}

/**
 * Все служебные пузыри ленты в одном разговоре: новые и старые формы вводов,
 * будильники, сторож машины, контекст, лимиты и сообщения другой сессии.
 */
export function seedSystemBubbles(
  api: FakeHubApi,
  conversationId: string,
  group: SystemBubbleGroup = 'all'
): Seed {
  const child = seedConversation(api, { title: 'Сборка' });
  const ownerInput = api.id();
  const items: HistoryInput[] = [
    input(
      api,
      1,
      `[canterlot] Alarm "gate" fired at 2026-11-10 12:00 UTC (set 39 min ago).\n\n${AUTOMATION}`,
      {
        kind: 'alarm',
        alarm_id: 'a1',
        chain_depth: 0,
        event: {
          kind: 'alarm',
          alarm_id: 'a1',
          alarm_key: 'gate',
          fired_reason: 'time',
          armed_at: EARLIER,
          condition: { kind: 'at', at: AT },
          note: 'Гейт для волны C: не начинать миграцию до сборки.'
        }
      }
    ),
    input(
      api,
      2,
      `[canterlot] Alarm "release" fired: file /tmp/gate.rc appeared (2 KB).\n\n${AUTOMATION}`,
      {
        kind: 'alarm',
        alarm_id: 'a2',
        chain_depth: 0,
        event: {
          kind: 'alarm',
          alarm_id: 'a2',
          alarm_key: 'release',
          fired_reason: 'condition',
          armed_at: EARLIER,
          condition: { kind: 'file', path: '/tmp/gate.rc' },
          alarm_path: '/tmp/gate.rc',
          alarm_size: 2048
        }
      }
    ),
    input(
      api,
      3,
      `[canterlot] Alarm "watch" timed out after 1 h: session ${child} is still stalled.\n\n${AUTOMATION}`,
      {
        kind: 'alarm',
        alarm_id: 'a3',
        chain_depth: 0,
        event: {
          kind: 'alarm',
          alarm_id: 'a3',
          alarm_key: 'watch',
          fired_reason: 'timed_out',
          armed_at: EARLIER,
          condition: { kind: 'session_state', session_ids: [child], states: ['stalled'] },
          sessions: [{ id: child, title: 'Сборка', state: 'stalled' }]
        }
      }
    ),
    input(
      api,
      4,
      `[canterlot] 2 alarms fired.\n\n- Alarm "gate" fired\n- Alarm "disk" fired\n\n${AUTOMATION}`,
      {
        kind: 'alarm',
        alarm_id: 'a4',
        chain_depth: 0,
        event: {
          kind: 'alarm',
          alarm_key: 'gate',
          fired_reason: 'time',
          armed_at: EARLIER,
          condition: { kind: 'at' },
          alarms: [
            { alarm_id: 'a4', alarm_key: 'gate', fired_reason: 'time', armed_at: EARLIER, condition: { kind: 'at' } },
            { alarm_id: 'a5', alarm_key: 'disk', fired_reason: 'condition', armed_at: EARLIER, condition: { kind: 'machine' } }
          ]
        }
      }
    ),
    input(api, 5, '[canterlot] Alarm "old" fired.', {
      kind: 'alarm',
      alarm_id: 'a6',
      chain_depth: 0,
      event: { kind: 'alarm', alarm_id: 'a6', alarm_key: 'old', fired_reason: 'time' }
    }),
    input(
      api,
      6,
      `[canterlot] Machine "dev-vm" is low on disk: 23 GB free (8%). Builds and new sessions may fail. Largest build caches: /w/target 77 GB.\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: conversationId,
        title: 'Оркестратор',
        chain_depth: 0,
        event: {
          kind: 'machine_disk_low',
          session_id: conversationId,
          machine_id: 'm1',
          machine_name: 'dev-vm',
          state: 'working',
          details: {
            free_gb: 23,
            free_pct: 8,
            hotspots: [
              { path: '/w/target', kind: 'target', bytes: 77_000_000_000, incremental_bytes: 31_000_000_000, newest_at: AT, title: null },
              { path: '/data/worktrees/x/target', kind: 'target', bytes: 12_000_000_000, incremental_bytes: 4_000_000_000, newest_at: AT, title: 'Баннер связи' }
            ]
          }
        }
      }
    ),
    input(
      api,
      7,
      `[canterlot] Machine "dev-vm" is critically low on disk: 3 GB free (1%). Builds and new sessions may fail.\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: conversationId,
        title: 'Оркестратор',
        chain_depth: 0,
        event: {
          kind: 'machine_disk_critical',
          session_id: conversationId,
          machine_id: 'm1',
          machine_name: 'dev-vm',
          state: 'working',
          details: { free_gb: 3, free_pct: 1, hotspots: [] }
        }
      }
    ),
    input(
      api,
      8,
      `[canterlot] Machine "dev-vm" is low on memory: 3.1 GB available of 32 GB. Largest sessions: «Toolkit» 6.2 GB.\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: conversationId,
        title: 'Оркестратор',
        chain_depth: 0,
        event: {
          kind: 'machine_mem_low',
          session_id: conversationId,
          machine_id: 'm1',
          machine_name: 'dev-vm',
          state: 'working',
          details: {
            available_bytes: 3_100_000_000,
            total_bytes: 32_000_000_000,
            heaviest: [
              { execution_id: 'e1', title: 'Toolkit', rss_bytes: 6_200_000_000 },
              { execution_id: 'e2', title: null, rss_bytes: 3_000_000_000 }
            ]
          }
        }
      }
    ),
    input(
      api,
      9,
      `[canterlot] 2 processes outlived their sessions on "dev-vm" (cargo 2; 1 GB RSS): from «Сборка».\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: conversationId,
        title: 'Оркестратор',
        chain_depth: 0,
        event: {
          kind: 'machine_processes_leaked',
          session_id: conversationId,
          machine_id: 'm1',
          machine_name: 'dev-vm',
          state: 'working',
          details: {
            leaked: [
              { execution_id: 'e1', comm: 'cargo', rss_bytes: 600_000_000, title: 'Сборка' },
              { execution_id: 'e1', comm: 'cargo', rss_bytes: 380_000_000, title: 'Сборка' }
            ]
          }
        }
      }
    ),
    input(
      api,
      10,
      `[canterlot] Machine "dev-vm": session «Сборка» (id ${child}) has held the build lease for 95 min.\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: conversationId,
        title: 'Оркестратор',
        chain_depth: 0,
        event: {
          kind: 'lease_long_hold',
          session_id: conversationId,
          machine_id: 'm1',
          machine_name: 'dev-vm',
          state: 'working',
          details: { holder: child, holder_title: 'Сборка', conversation_id: child, lease_kind: 'build', held_minutes: 95 }
        }
      }
    ),
    input(
      api,
      11,
      `[canterlot] Machine "dev-vm" is low on disk: 12 GB free (4%).\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: conversationId,
        title: 'Оркестратор',
        chain_depth: 0,
        event: { kind: 'machine_disk_low', session_id: conversationId, machine_id: 'm1', machine_name: 'dev-vm' }
      }
    ),
    input(
      api,
      12,
      `[canterlot] Session ${conversationId} context is 85% full (850000 of 1000000 tokens); ask it to commit and summarize.\n\n${AUTOMATION}`,
      { kind: 'event', chain_depth: 0, event: { kind: 'context_high', session_id: conversationId, detail: '85%' } }
    ),
    input(
      api,
      13,
      `[canterlot] Session ${conversationId} context is 92% full (920000 of 1000000 tokens); ask it to commit and summarize.\n\n${AUTOMATION}`,
      { kind: 'event', chain_depth: 0, event: { kind: 'context_high', session_id: conversationId, detail: '92%' } }
    ),
    input(
      api,
      14,
      'Лимиты восстановлены, продолжай с того места, где остановился.',
      {
        kind: 'event',
        chain_depth: 0,
        event: { kind: 'limit_resumed', provider: 'claude', window: 'weekly', resets_at: RESET }
      }
    ),
    input(
      api,
      15,
      `[canterlot] Session "Сборка" (id ${child}) is now quota_paused.\n\n${AUTOMATION}`,
      {
        kind: 'event',
        from: child,
        title: 'Сборка',
        harness: 'claude_code',
        model: 'claude-sonnet-5',
        chain_depth: 1,
        event: {
          kind: 'quota_paused',
          state: 'quota_paused',
          session_id: child,
          provider: 'claude',
          window: 'weekly',
          resets_at: RESET,
          until: RESET
        }
      }
    ),
    input(
      api,
      16,
      `[canterlot] Session "Сборка" (id ${child}) was paused by the claude weekly limit; the limit has reset, but its turn was cut and there is no live execution, so it will not continue on its own. Send it a message to continue, or stop it.`,
      {
        kind: 'event',
        from: child,
        title: 'Сборка',
        chain_depth: 0,
        event: {
          kind: 'quota_paused',
          session_id: child,
          provider: 'claude',
          window: 'weekly',
          resets_at: RESET,
          reason: 'no_live_execution'
        }
      }
    ),
    input(
      api,
      17,
      '[canterlot] Message from session "Сборка" (id ' +
        child +
        ', claude_code, model sonnet-5). This is another agent, not the owner.\n\nПроверь миграцию и не забудь про индексы.',
      {
        kind: 'message',
        from: child,
        title: 'Сборка',
        harness: 'claude_code',
        model: 'claude-sonnet-5',
        chain_depth: 1,
        sha256: 'a'.repeat(64)
      }
    ),
    input(
      api,
      18,
      `[canterlot] You were started by session "Оркестратор" (id ${conversationId}, claude_code, model opus). Your final message goes back to it. It is an agent, not the owner.\n\n` +
        Array.from({ length: 18 }, (_, index) => `Шаг ${index + 1}: проверь и закоммить.`).join('\n'),
      {
        kind: 'spawn',
        from: conversationId,
        title: 'Оркестратор',
        harness: 'claude_code',
        model: 'opus',
        chain_depth: 0
      }
    ),
    {
      ...input(api, 19, 'Моё сообщение', { kind: 'message', from: child, title: 'Сборка', chain_depth: 1 }),
      id: ownerInput,
      content: [{ type: 'text', text: 'Моё сообщение' }],
      origin: null
    } as HistoryInput,
    input(
      api,
      20,
      `[canterlot] Session ${child} context is 88% full.\n\n${AUTOMATION}`,
      { kind: 'event', chain_depth: 0, event: { kind: 'context_high', session_id: child, detail: '88%' } }
    )
  ];
  const groups: SystemBubbleGroup[] = [
    'alarm', 'alarm', 'alarm', 'alarm', 'alarm',
    'machine', 'machine', 'machine', 'machine', 'machine', 'machine',
    'context', 'context',
    'limit', 'limit', 'limit',
    'message', 'message', 'message',
    'context'
  ];
  api.histories.set(
    conversationId,
    items.filter((_, index) => group === 'all' || groups[index] === group)
  );
  return { child, ownerInput };
}
