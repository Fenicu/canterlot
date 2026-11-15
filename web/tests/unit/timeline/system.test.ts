import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';
import {
  alarmView,
  apiErrorView,
  contextView,
  limitView,
  machineView,
  sessionMessageView
} from '#lib/timeline/system.js';
import type { InputOrigin } from '#lib/stores/inputs.js';

const CREATED = Date.parse('2026-11-10T12:00:00.000Z');

function origin(value: Record<string, unknown>): InputOrigin {
  return value as InputOrigin;
}

describe('timeline/system alarm', () => {
  it('a time alarm names its key, reason and armed moment', () => {
    const view = alarmView(
      origin({
        kind: 'alarm',
        event: {
          kind: 'alarm',
          alarm_key: 'gate',
          fired_reason: 'time',
          armed_at: '2026-11-10T10:21:00.000Z',
          condition: { kind: 'at', at: '2026-11-10T12:00:00.000Z' },
          note: 'Гейт для волны C'
        }
      }),
      'полный текст',
      CREATED
    );
    expect(view).toMatchObject({
      kind: 'alarm',
      icon: 'alarm',
      title: 'Будильник «gate»',
      reason: 'Пора',
      tone: 'neutral',
      note: 'Гейт для волны C',
      text: 'полный текст'
    });
    expect(view?.facts).toEqual([
      { label: 'Поставлен', value: `${fmt.clock(Date.parse('2026-11-10T10:21:00.000Z'))} · ждал 1 ч 39 мин` }
    ]);
    expect(view?.rows).toEqual([]);
  });

  it('a file alarm shows the path and size from the structure', () => {
    const view = alarmView(
      origin({
        kind: 'alarm',
        event: {
          kind: 'alarm',
          alarm_key: 'release',
          fired_reason: 'condition',
          armed_at: '2026-11-10T11:00:00.000Z',
          condition: { kind: 'file', path: '/tmp/gate.rc' },
          alarm_path: '/tmp/gate.rc',
          alarm_size: 2048
        }
      }),
      'текст',
      CREATED
    );
    expect(view?.reason).toBe('Появился файл /tmp/gate.rc (2 КиБ)');
  });

  it('a watchdog timeout is a warning', () => {
    const view = alarmView(
      origin({
        kind: 'alarm',
        event: {
          kind: 'alarm',
          alarm_key: 'watch',
          fired_reason: 'timed_out',
          armed_at: '2026-11-10T11:00:00.000Z',
          condition: { kind: 'session_state', session_ids: [], states: [] }
        }
      }),
      'текст',
      CREATED
    );
    expect(view?.reason).toBe('Истёк срок ожидания');
    expect(view?.tone).toBe('warning');
  });

  it('session targets become rows with state words and links', () => {
    const view = alarmView(
      origin({
        kind: 'alarm',
        event: {
          kind: 'alarm',
          alarm_key: 'watch',
          fired_reason: 'condition',
          armed_at: '2026-11-10T11:00:00.000Z',
          condition: { kind: 'session_state', session_ids: ['s1'], states: ['idle'] },
          sessions: [{ id: 's1', title: 'Сборка', state: 'idle' }]
        }
      }),
      'текст',
      CREATED
    );
    expect(view?.rows).toEqual([{ title: 'Сборка', value: 'завершена', href: '/c/s1' }]);
  });

  it('a batch lists one row per alarm', () => {
    const view = alarmView(
      origin({
        kind: 'alarm',
        event: {
          kind: 'alarm',
          alarm_key: 'gate',
          armed_at: '2026-11-10T11:00:00.000Z',
          condition: { kind: 'at' },
          alarms: [
            { alarm_id: 'a1', alarm_key: 'gate', condition: { kind: 'at' } },
            { alarm_id: 'a2', alarm_key: 'disk', fired_reason: 'timed_out', condition: { kind: 'session_state' } }
          ]
        }
      }),
      'текст',
      CREATED
    );
    expect(view?.title).toBe('Сработало 2 будильника');
    expect(view?.tone).toBe('warning');
    expect(view?.rows).toEqual([
      { title: 'Будильник «gate»', value: 'Пора' },
      { title: 'Будильник «disk»', value: 'Истёк срок ожидания' }
    ]);
  });

  it('an old alarm without the new fields falls back to the thin line', () => {
    expect(
      alarmView(
        origin({ kind: 'alarm', event: { kind: 'alarm', alarm_key: 'gate', fired_reason: 'time' } }),
        'текст',
        CREATED
      )
    ).toBeNull();
  });
});

describe('timeline/system machine', () => {
  it('low disk shows free space and the largest caches', () => {
    const view = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'machine_disk_low',
          machine_id: 'm1',
          machine_name: 'dev-vm',
          details: {
            free_gb: 23,
            free_pct: 8,
            hotspots: [{ path: '/w/target', bytes: 77_000_000_000, incremental_bytes: 31_000_000_000 }]
          }
        }
      }),
      'английский текст'
    );
    expect(view).toMatchObject({
      kind: 'machine',
      icon: 'server',
      title: 'Машина dev-vm: мало места',
      tone: 'neutral',
      action: { kind: 'link', label: 'Открыть машины', href: '/machines' }
    });
    expect(view?.facts).toEqual([{ label: 'Свободно', value: '21,4 ГиБ (8 %)' }]);
    expect(view?.rows).toEqual([{ title: '/w/target', value: '71,7 ГиБ' }]);
  });

  it('critical disk and low memory are warnings', () => {
    expect(
      machineView(
        origin({
          kind: 'event',
          event: { kind: 'machine_disk_critical', machine_name: 'm', details: { free_gb: 3, free_pct: 1 } }
        }),
        'текст'
      )?.tone
    ).toBe('warning');
    const memory = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'machine_mem_low',
          machine_name: 'dev-vm',
          details: { available_bytes: 3_100_000_000, total_bytes: 32_000_000_000, heaviest: [{ title: 'Toolkit', rss_bytes: 6_200_000_000 }] }
        }
      }),
      'текст'
    );
    expect(memory?.tone).toBe('warning');
    expect(memory?.facts).toEqual([{ label: 'Память', value: '2,9 ГиБ из 29,8 ГиБ' }]);
    expect(memory?.rows).toEqual([{ title: 'Toolkit', value: '5,8 ГиБ' }]);
  });

  it('leaked processes and a long lease name the culprit', () => {
    const leaked = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'machine_processes_leaked',
          machine_name: 'dev-vm',
          details: { leaked: [{ comm: 'cargo', title: 'Toolkit', rss_bytes: 600_000_000 }] }
        }
      }),
      'текст'
    );
    expect(leaked?.title).toBe('Машина dev-vm: остались процессы');
    expect(leaked?.rows).toEqual([{ title: 'cargo · «Toolkit»', value: '572,2 МиБ' }]);
    const lease = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'lease_long_hold',
          machine_name: 'dev-vm',
          details: { holder: 'owner', lease_kind: 'build', held_minutes: 95 }
        }
      }),
      'текст'
    );
    expect(lease?.title).toBe('Машина dev-vm: аренда занята долго');
    expect(lease?.facts).toEqual([
      { label: 'Держатель', value: 'терминал владельца' },
      { label: 'Аренда', value: 'build' },
      { label: 'Держит', value: '95 мин' }
    ]);
  });

  it('machine_bubbles_use_binary_units_only', () => {
    const disk = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'machine_disk_low',
          machine_name: 'dev-vm',
          details: {
            free_gb: 23,
            free_pct: 8,
            hotspots: [{ path: '/w/target', bytes: 77_000_000_000 }]
          }
        }
      }),
      'текст'
    );
    const memory = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'machine_mem_low',
          machine_name: 'dev-vm',
          details: {
            available_bytes: 3_100_000_000,
            total_bytes: 32_000_000_000,
            heaviest: [{ title: 'Toolkit', rss_bytes: 6_200_000_000 }]
          }
        }
      }),
      'текст'
    );
    const leaked = machineView(
      origin({
        kind: 'event',
        event: {
          kind: 'machine_processes_leaked',
          machine_name: 'dev-vm',
          details: { leaked: [{ comm: 'cargo', title: 'Toolkit', rss_bytes: 600_000_000 }] }
        }
      }),
      'текст'
    );
    for (const view of [disk, memory, leaked]) {
      expect(view).not.toBeNull();
      const strings = [
        view!.title,
        view!.reason ?? '',
        ...view!.facts.flatMap((fact) => [fact.label, fact.value]),
        ...view!.rows.flatMap((row) => [row.title, row.value ?? ''])
      ];
      for (const value of strings)
        expect(value.includes(' ГБ') && value.includes(' ГиБ')).toBe(false);
      expect(strings.join(' ')).not.toContain(' ГБ');
    }
    expect(JSON.stringify([disk, memory])).toContain('ГиБ');
  });

  it('an old machine event without details falls back', () => {
    expect(
      machineView(origin({ kind: 'event', event: { kind: 'machine_disk_low', machine_id: 'm1' } }), 'текст')
    ).toBeNull();
  });
});

describe('timeline/system context', () => {
  it('names the percentage and offers compaction only in the own session', () => {
    const event = { kind: 'context_high', session_id: 'c1', detail: '85%' };
    const own = contextView(origin({ kind: 'event', event }), 'текст', 'c1');
    expect(own).toMatchObject({
      title: 'Контекст заполнен на 85 %',
      tone: 'neutral',
      action: { kind: 'compact', label: 'Сжать контекст' }
    });
    expect(contextView(origin({ kind: 'event', event }), 'текст', 'other')?.action).toBeUndefined();
  });

  it('is a warning from ninety percent', () => {
    expect(
      contextView(
        origin({ kind: 'event', event: { kind: 'context_high', session_id: 'c1', detail: '92%' } }),
        'текст',
        'c1'
      )?.tone
    ).toBe('warning');
  });

  it('an event without a readable percentage is not a bubble', () => {
    expect(
      contextView(origin({ kind: 'event', event: { kind: 'context_high', session_id: 'c1' } }), 'текст', 'c1')
    ).toBeNull();
  });
});

describe('timeline/system limits', () => {
  const NOW = CREATED;

  it('the auto-continue names the provider, window and reset', () => {
    const view = limitView(
      origin({
        kind: 'event',
        event: {
          kind: 'limit_resumed',
          provider: 'claude',
          window: 'weekly',
          resets_at: '2026-11-10T04:00:00.000Z'
        }
      }),
      'текст'
    );
    expect(view).toMatchObject({
      kind: 'limit_resumed',
      icon: 'hourglass',
      title: 'Лимит Claude сброшен — продолжаю',
      tone: 'neutral'
    });
    expect(view?.facts).toEqual([
      { label: 'Окно', value: 'неделя' },
      { label: 'Сброс', value: fmt.clock(Date.parse('2026-11-10T04:00:00.000Z')) }
    ]);
  });

  it('a paused delegate has a link and the reset time', () => {
    const view = limitView(
      origin({
        kind: 'event',
        title: 'Сборка',
        event: { kind: 'quota_paused', session_id: 's1', provider: 'claude', window: 'weekly', resets_at: '2026-11-10T04:00:00.000Z' }
      }),
      'текст'
    );
    expect(view).toMatchObject({
      kind: 'quota_paused',
      title: `Помощник «Сборка» упёрся в лимит Claude (неделя) · сброс ${fmt.clock(Date.parse('2026-11-10T04:00:00.000Z'))}`,
      titleHref: '/c/s1',
      tone: 'neutral'
    });
    expect(view?.title).not.toContain('undefined');
  });

  it('a stuck delegate note keeps the hint as text', () => {
    const view = limitView(
      origin({
        kind: 'event',
        title: 'Сборка',
        event: { kind: 'quota_paused', session_id: 's1', provider: 'claude', window: 'five_hour', reason: 'no_live_execution' }
      }),
      'текст'
    );
    expect(view).toMatchObject({
      kind: 'quota_note',
      title: 'Лимит сброшен, но «Сборка» сам не продолжит',
      titleHref: '/c/s1',
      reason: 'Отправьте ему сообщение или остановите'
    });
    expect(view?.action).toBeUndefined();
    expect(NOW).toBeGreaterThan(0);
  });
});

describe('timeline/system api error', () => {
  const HARNESS = { id: 'claude_code', provider: 'claude' };
  const VERIFY =
    "403: API Error: Your account can't receive replies from Claude right now. You can find the next step in the Anthropic Verification Portal: https://portal.anthropic.com/verify?org=01a1dfb1-10bc-7230-8b59-a74abd632567";

  it('a 403 verification error is a danger bubble with the provider link and the folded text', () => {
    const view = apiErrorView({ kind: 'error', text: VERIFY }, HARNESS);
    expect(view).toMatchObject({
      kind: 'api_error',
      icon: 'warning',
      title: 'Claude: аккаунт требует проверки',
      tone: 'danger',
      reason: 'Сессия ждёт, пока аккаунт не пройдёт проверку на странице провайдера.',
      action: {
        kind: 'link',
        label: 'Открыть страницу провайдера',
        href: 'https://portal.anthropic.com/verify?org=01a1dfb1-10bc-7230-8b59-a74abd632567',
        external: true
      },
      foldLabel: 'Подробнее',
      plain: true,
      text: VERIFY
    });
    expect(view?.facts).toEqual([]);
    expect(view?.rows).toEqual([]);
  });

  it('a 401 asks for the login again without a link', () => {
    const view = apiErrorView({ kind: 'error', text: '401: Please run /login' }, HARNESS);
    expect(view).toMatchObject({
      title: 'Claude: нужен повторный вход',
      tone: 'danger',
      reason: 'Провайдер не принял вход. Проверьте вход в аккаунт на машине и повторите запрос.'
    });
    expect(view?.action).toBeUndefined();
  });

  it('rate limits are warnings and an overload names the provider without a colon', () => {
    expect(apiErrorView({ kind: 'error', text: '429: usage_limit_reached' }, HARNESS)).toMatchObject({
      title: 'Claude: слишком много запросов',
      tone: 'warning'
    });
    expect(apiErrorView({ kind: 'error', text: '529: API Error: Overloaded' }, HARNESS)).toMatchObject({
      title: 'Claude перегружен',
      tone: 'warning'
    });
  });

  it('a provider side failure and a rejected request keep their own names', () => {
    expect(apiErrorView({ kind: 'error', text: '503: unavailable' }, HARNESS)).toMatchObject({
      title: 'Claude: ошибка на стороне провайдера',
      tone: 'danger'
    });
    expect(
      apiErrorView({ kind: 'error', text: '400: API Error: 400 invalid_request_error: bad' }, HARNESS)
    ).toMatchObject({ title: 'Claude: ошибка API 4xx', tone: 'danger' });
  });

  it('codex and opencode keep their own provider names', () => {
    expect(
      apiErrorView(
        { kind: 'error', text: 'Сервер перегружен (HTTP 503)' },
        { id: 'codex', provider: 'codex' }
      )
    ).toMatchObject({ title: 'Codex перегружен', tone: 'warning' });
    expect(
      apiErrorView(
        { kind: 'error', text: 'Ошибка OpenCode (provider.auth): invalid api key' },
        { id: 'opencode', provider: 'opencode' }
      )
    ).toMatchObject({ title: 'OpenCode: нужен повторный вход' });
  });

  it('a non provider error stays a notice inside the card', () => {
    for (const text of [
      'Ошибка Codex',
      'Запуск остановлен: делегированной сессии доступны чужие серверы MCP (x).',
      'trajectory not found'
    ])
      expect(apiErrorView({ kind: 'error', text }, HARNESS), text).toBeNull();
    expect(apiErrorView({ kind: 'warning', text: '401: Please run /login' }, HARNESS)).toBeNull();
    expect(apiErrorView({ kind: 'text', text: VERIFY }, HARNESS)).toBeNull();
  });
});

describe('timeline/system session message', () => {
  it('a message becomes an agent-side bubble with the sender and model', () => {
    const view = sessionMessageView(
      origin({
        kind: 'message',
        from: 'p1',
        title: 'Оркестратор',
        model: 'claude-sonnet-5',
        chain_depth: 1
      }),
      '[canterlot] Message from session "Оркестратор" (id p1, claude_code, model claude-sonnet-5). This is another agent, not the owner.\n\nСделай миграцию'
    );
    expect(view).toEqual({
      kind: 'message',
      title: 'Сессия «Оркестратор»',
      model: 'Sonnet 5',
      href: '/c/p1',
      body: 'Сделай миграцию',
      foldable: false
    });
  });

  it('a spawn brief keeps the whole body and is folded by height', () => {
    const lines = Array.from({ length: 20 }, (_, index) => `строка ${index + 1}`).join('\n');
    const view = sessionMessageView(
      origin({ kind: 'spawn', from: 'p1', title: 'Оркестратор', model: 'opus', chain_depth: 0 }),
      `[canterlot] You were started by session "Оркестратор" (id p1, claude_code, model opus).\n\n${lines}`
    );
    expect(view?.title).toBe('Задание от сессии «Оркестратор»');
    expect(view?.body).toBe(lines);
    expect(view?.foldable).toBe(true);
  });

  it('a deleted sender loses the link and the model', () => {
    const view = sessionMessageView(
      origin({ kind: 'message', sender_deleted: true, chain_depth: 1 }),
      '[canterlot] Message from a session that was deleted. This is another agent, not the owner.\n\nтекст'
    );
    expect(view).toEqual({
      kind: 'message',
      title: 'Сессия удалена',
      model: null,
      href: null,
      body: 'текст',
      foldable: false
    });
  });

  it('owner and event inputs are not session messages', () => {
    expect(sessionMessageView(null, 'текст')).toBeNull();
    expect(sessionMessageView(origin({ kind: 'event', chain_depth: 0 }), 'текст')).toBeNull();
  });
});
