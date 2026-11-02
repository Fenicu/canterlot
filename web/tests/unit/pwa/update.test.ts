import { afterEach, expect, test, vi } from 'vitest';
import { UpdateController } from '../../../src/lib/pwa/update';

type WorkerLike = EventTarget & { state: ServiceWorkerState; postMessage: ReturnType<typeof vi.fn> };

function worker(state: ServiceWorkerState, onPost?: () => void): WorkerLike {
  return Object.assign(new EventTarget(), {
    state,
    postMessage: vi.fn(() => onPost?.())
  }) as unknown as WorkerLike;
}

function setup(init: { waiting?: WorkerLike | null; installing?: WorkerLike | null } = {}) {
  const reg = Object.assign(new EventTarget(), {
    waiting: init.waiting ?? null,
    installing: init.installing ?? null,
    update: vi.fn(async () => {})
  });
  const container = new EventTarget();
  const reload = vi.fn();
  const flushDrafts = vi.fn(async () => {});
  const make = (overrides: Record<string, unknown> = {}) => new UpdateController(
    reg as unknown as ServiceWorkerRegistration,
    { isSafe: () => true, hidden: () => false, flushDrafts, serviceWorker: container, reload, ...overrides }
  );
  return { reg, container, reload, flushDrafts, make };
}

afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });

test('version_check_without_a_worker_is_surfaced_and_one_press_reloads', async () => {
  const { reg, reload, make } = setup();
  const ctl = make();
  expect(ctl.state).toBe('idle');
  ctl.notify();
  expect(ctl.state).toBe('available');
  expect(ctl.message).toBe('Доступна новая версия');
  await ctl.apply(true);
  expect(reg.update).toHaveBeenCalledOnce();
  await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce());
  ctl.destroy();
});

test('one_press_waits_for_an_installing_worker_then_activates_it', async () => {
  const candidate = worker('installing');
  const { container, reload, make } = setup({ installing: candidate });
  const ctl = make();
  expect(ctl.state).toBe('available');
  await ctl.apply(true);
  candidate.state = 'installed';
  candidate.dispatchEvent(new Event('statechange'));
  await vi.waitFor(() => expect(candidate.postMessage).toHaveBeenCalledWith({ type: 'SKIP_WAITING' }));
  expect(reload).not.toHaveBeenCalled();
  container.dispatchEvent(new Event('controllerchange'));
  await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce());
  expect(ctl.state).toBe('applying');
  ctl.destroy();
});

test('a_synchronous_controllerchange_during_skip_waiting_still_reloads', async () => {
  const { container, reload } = setup();
  const waiting = worker('installed', () => container.dispatchEvent(new Event('controllerchange')));
  const reg = Object.assign(new EventTarget(), { waiting, installing: null, update: vi.fn(async () => {}) });
  const ctl = new UpdateController(reg as unknown as ServiceWorkerRegistration,
    { isSafe: () => true, hidden: () => false, flushDrafts: async () => {}, serviceWorker: container, reload });
  await ctl.apply(true);
  await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce());
  ctl.destroy();
});

test('missing_controllerchange_is_bounded_by_the_activation_timeout', async () => {
  vi.useFakeTimers();
  const waiting = worker('installed');
  const { reload, make } = setup({ waiting });
  const ctl = make();
  await ctl.apply(true);
  expect(waiting.postMessage).toHaveBeenCalledOnce();
  expect(reload).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(reload).toHaveBeenCalledOnce();
  ctl.destroy();
});

test('a_failed_update_check_is_explained_and_the_next_press_activates', async () => {
  const { reg, reload, make } = setup();
  reg.update.mockRejectedValueOnce(new Error('offline'));
  const ctl = make();
  await ctl.apply(true);
  expect(ctl.state).toBe('failed');
  expect(ctl.message).toContain('Не удалось проверить обновление');
  expect(reload).not.toHaveBeenCalled();
  const waiting = worker('installed');
  reg.waiting = waiting;
  reg.update.mockResolvedValue(undefined);
  await ctl.apply(true);
  expect(waiting.postMessage).toHaveBeenCalledWith({ type: 'SKIP_WAITING' });
  ctl.destroy();
});

test('a_draft_flush_failure_is_explained_and_does_not_activate', async () => {
  const waiting = worker('installed');
  const { reload, make } = setup({ waiting });
  const flushDrafts = vi.fn(async () => { throw new Error('idb'); });
  const ctl = make({ flushDrafts });
  await ctl.apply(true);
  expect(ctl.state).toBe('failed');
  expect(ctl.message).toContain('черновик');
  expect(waiting.postMessage).not.toHaveBeenCalled();
  expect(reload).not.toHaveBeenCalled();
  ctl.destroy();
});
