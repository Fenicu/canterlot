import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

const api = vi.hoisted(() => ({
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  deleteSchedule: vi.fn(),
  scheduleRuns: vi.fn(async () => ({ data: { items: [] } })),
  listSchedules: vi.fn(),
  eventsRange: vi.fn(async () => ({ data: { head: '1', events: [] } }))
}));
vi.mock('#lib/api/client.js', async (original) => ({ ...(await original<typeof import('#lib/api/client.js')>()), ...api }));
// Подписка сразу отдаёт снимок списка, как живое соединение после подключения.
vi.mock('#lib/app/browser.js', () => ({
  browserRuntime: async () => ({
    list: { items: new Map() },
    connection: {
      subscribe: (_scope: string, applier: { loadSnapshot(): Promise<{ data: unknown }>; applySnapshot(data: unknown): void }) => {
        void applier.loadSnapshot().then(({ data }) => applier.applySnapshot(data));
        return { release() {} };
      }
    }
  })
}));

const { default: SchedulesScreen } = await import('#lib/screens/settings/SchedulesScreen.svelte');

const schedule = (id: string, name: string, enabled: boolean) => ({
  id, name, enabled, cron: '0 9 * * *', timezone: 'UTC', next_fire_at: '2026-11-26T09:00:00Z', revision: 1,
  target: { kind: 'new_session' }
});

/** Запрос, исход которого тест выбирает сам. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe('settings/schedule switch', () => {
  let instance: ReturnType<typeof mount> | undefined;

  beforeEach(() => {
    for (const mock of Object.values(api)) mock.mockClear();
    api.listSchedules.mockResolvedValue({ data: [schedule('a', 'Разбор коммитов', true), schedule('b', 'Ночной отчёт', true)] });
  });

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    document.body.textContent = '';
  });

  async function show(): Promise<HTMLInputElement[]> {
    instance = mount(SchedulesScreen, { target: document.body });
    flushSync();
    await vi.waitFor(() => expect(document.querySelectorAll('[data-schedule-row]')).toHaveLength(2));
    return [...document.querySelectorAll<HTMLInputElement>('input[role="switch"]')];
  }

  function tap(input: HTMLInputElement): void {
    input.click();
    flushSync();
  }

  it('shows the real state again when the request fails', async () => {
    const pause = deferred<never>();
    api.pauseSchedule.mockReturnValue(pause.promise);
    const [first] = await show();

    tap(first!);
    expect(first!.checked).toBe(false);
    pause.reject(new Error('offline'));

    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain('Не удалось изменить расписание'));
    // Расписание осталось включённым, и выключатель не должен показывать обратное.
    expect(first!.checked).toBe(true);
    expect(first!.disabled).toBe(false);
  });

  it('does not let a second switch flip while a request is in flight', async () => {
    const pause = deferred<{ data: { enabled: boolean } }>();
    api.pauseSchedule.mockReturnValue(pause.promise);
    const [first, second] = await show();

    tap(first!);
    expect(second!.disabled).toBe(true);
    tap(second!);

    expect(second!.checked).toBe(true);
    expect(api.pauseSchedule).toHaveBeenCalledTimes(1);

    api.listSchedules.mockResolvedValue({ data: [schedule('a', 'Разбор коммитов', false), schedule('b', 'Ночной отчёт', true)] });
    pause.resolve({ data: { enabled: false } });
    await vi.waitFor(() => expect(second!.disabled).toBe(false));
    expect(first!.checked).toBe(false);
    expect(second!.checked).toBe(true);
  });
});
