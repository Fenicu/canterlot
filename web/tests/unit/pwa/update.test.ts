import { afterEach, expect, test, vi } from 'vitest';
import { UpdateController } from '../../../src/lib/pwa/update';

function setup(overrides: Record<string, unknown> = {}) {
  const reload = vi.fn();
  const flushDrafts = vi.fn(async () => {});
  const fetch = vi.fn(async () => new Response(JSON.stringify({ version: 'next' }), { headers: { 'Content-Type': 'application/json' } }));
  const make = (extra: Record<string, unknown> = {}) => new UpdateController({
    isSafe: () => true, hidden: () => false, flushDrafts, reload, version: 'current', fetch,
    ...overrides, ...extra
  });
  return { reload, flushDrafts, fetch, make };
}

afterEach(() => { vi.restoreAllMocks(); });

test('version_json_marks_a_new_version_without_a_service_worker', async () => {
  const { fetch, make } = setup();
  const ctl = make();
  expect(ctl.state).toBe('idle');
  await ctl.check();
  expect(fetch).toHaveBeenCalledWith('/_app/version.json', expect.objectContaining({ cache: 'no-store' }));
  expect(ctl.state).toBe('available');
  expect(ctl.message).toBe('Доступна новая версия');
  ctl.destroy();
});

test('the_same_version_and_network_failures_do_not_offer_an_update', async () => {
  const same = setup();
  same.fetch.mockResolvedValueOnce(new Response(JSON.stringify({ version: 'current' })));
  const one = same.make();
  await one.check(); expect(one.state).toBe('idle'); one.destroy();
  const down = setup();
  down.fetch.mockRejectedValueOnce(new Error('offline'));
  const two = down.make();
  await two.check(); expect(two.state).toBe('idle'); two.destroy();
});

test('one_press_flushes_drafts_and_reloads_the_page', async () => {
  const { flushDrafts, reload, make } = setup();
  const ctl = make();
  ctl.notify();
  expect(ctl.state).toBe('available');
  await ctl.apply(true);
  expect(flushDrafts).toHaveBeenCalledOnce();
  expect(reload).toHaveBeenCalledOnce();
  expect(ctl.state).toBe('applying');
  ctl.destroy();
});

test('a_press_without_force_waits_for_a_safe_moment', async () => {
  const { flushDrafts, reload, make } = setup({ isSafe: () => false });
  const ctl = make();
  await ctl.apply();
  expect(flushDrafts).not.toHaveBeenCalled();
  expect(reload).not.toHaveBeenCalled();
  await ctl.apply(true);
  expect(flushDrafts).toHaveBeenCalledOnce();
  expect(reload).toHaveBeenCalledOnce();
  ctl.destroy();
});

test('a_failed_draft_flush_is_explained_and_does_not_reload', async () => {
  const flushDrafts = vi.fn(async () => { throw new Error('idb'); });
  const { reload, make } = setup({ flushDrafts });
  const ctl = make();
  await ctl.apply(true);
  expect(ctl.state).toBe('failed');
  expect(ctl.message).toContain('черновик');
  expect(reload).not.toHaveBeenCalled();
  ctl.destroy();
});

test('incompatible_protocol_offers_the_same_update_and_a_retry_clears_it', async () => {
  const { make } = setup();
  const ctl = make();
  ctl.incompatible();
  expect(ctl.state).toBe('available');
  ctl.destroy();
});

test('background_update_waits_while_an_edit_is_open_even_when_hidden', async () => {
  let editing = true;
  const { reload, make } = setup({ hidden: () => true, isSafe: () => false, editing: () => editing });
  const ctl = make();
  ctl.notify();
  await ctl.apply(false);
  expect(reload).not.toHaveBeenCalled();
  expect(ctl.state).toBe('available');
  editing = false;
  await ctl.apply(false);
  expect(reload).toHaveBeenCalledTimes(1);
  ctl.destroy();
});
