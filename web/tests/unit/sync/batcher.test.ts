import { expect, test, vi } from 'vitest';
import { FrameBatcher } from '../../../src/lib/sync/batcher';
import { Ledger, createProbe } from '../../../src/lib/perf/probe';
import { FakeClock } from '../../../testing/fake-hub';
import { append, chat, connectionHarness } from './helpers';

test('batcher_applies_one_batch_per_frame_and_preserves_order', () => {
  const clock = new FakeClock(), frames: number[] = [], values: number[] = [];
  const batcher = new FrameBatcher<number>(items => { frames.push(clock.now()); values.push(...items); }, { raf: clock.raf, cancelRaf: clock.cancelRaf, now: clock.now });
  for (let i = 0; i < 100; i++) batcher.push(i);
  clock.advance(64); expect(values).toEqual(Array.from({ length: 100 }, (_, i) => i));
  expect(new Set(frames).size).toBe(frames.length); expect(frames[0]).toBe(16); batcher.dispose(); expect(clock.pending).toBe(0);
});
test('batcher_time_slices_large_backlogs_within_the_frame_budget', () => {
  const clock = new FakeClock(), slices: number[] = [], values: number[] = []; let elapsed = 0;
  const batcher = new FrameBatcher<number>(items => { const before = elapsed; for (const i of items) { elapsed += 0.5; values.push(i); } slices.push(elapsed - before); },
    { raf: clock.raf, cancelRaf: clock.cancelRaf, now: () => elapsed, budgetMs: 8 });
  for (let i = 0; i < 1000; i++) batcher.push(i);
  clock.advance(16); expect(values.length).toBeLessThan(1000); clock.advance(2000);
  expect(Math.max(...slices)).toBeLessThanOrEqual(8.5); expect(slices.length).toBeGreaterThan(60);
  expect(values).toEqual(Array.from({ length: 1000 }, (_, i) => i)); batcher.dispose();
});
test('batcher_flushes_synchronously_on_pagehide', () => {
  const clock = new FakeClock(), apply = vi.fn();
  const batcher = new FrameBatcher<number>(apply, { raf: clock.raf, cancelRaf: clock.cancelRaf, now: clock.now });
  batcher.push(1); batcher.push(2); window.dispatchEvent(new Event('pagehide'));
  expect(apply).toHaveBeenCalledExactlyOnceWith([1, 2]); expect(clock.pending).toBe(0); batcher.dispose();
});
test('batcher_falls_back_to_timers_when_the_page_is_hidden', () => {
  const clock = new FakeClock(), apply = vi.fn(), raf = vi.fn(clock.raf);
  const batcher = new FrameBatcher<number>(apply, { raf, cancelRaf: clock.cancelRaf, now: clock.now, visible: () => false, schedule: clock.schedule });
  batcher.push(1); clock.advance(0); expect(apply).toHaveBeenCalledExactlyOnceWith([1]); expect(raf).not.toHaveBeenCalled(); batcher.dispose();
});
test('sleep_with_a_backlog_of_1000_events_is_applied_in_slices_and_in_order', async () => {
  const h = await connectionHarness(); h.lifecycle.suspend(); h.hub.chaos.blackhole(true);
  for (let n = 1; n <= 1000; n++) append(h.hub, n, n % 7 === 0);
  h.hub.flush(); await h.clock.advanceAsync(600_000); h.lifecycle.resume('visible', 600_000);
  await h.clock.advanceAsync(2000); expect(h.accepted.length).toBeLessThan(858);
  await h.clock.advanceAsync(1000);
  expect(h.subscription.cursor).toBe('1000');
  expect(h.accepted.map(item => item.event.kind === 'block_delta' ? item.event.payload.append : '').join('')).toBe(h.hub.log(chat).map(e => e.kind === 'block_delta' ? e.payload.append : '').join(''));
  expect(vi.mocked(h.applier.applyEvents).mock.calls.length).toBeGreaterThan(1); h.manager.dispose();
});
test('ledger_accounts_every_source_event_exactly_once', () => {
  const ledger = new Ledger();
  ledger.record(chat, '1', '10', 'snapshot'); ledger.record(chat, '11', '11', 'single'); ledger.record(chat, '12', '20', 'merged');
  ledger.record(chat, '21', '22', 'range'); ledger.record(chat, '23', '23', 'refetch');
  expect(ledger.verify(chat, '23')).toEqual({ ok: true, holes: [], doubled: [] });
  ledger.record(chat, '20', '21', 'range'); expect(ledger.verify(chat, '25')).toEqual({ ok: false, holes: [['24', '25']], doubled: [['20', '21']] });
  const disabled = createProbe(false); disabled.count('x'); disabled.record('x', 1); disabled.mark('x');
  expect(disabled.measure('x', 'x')).toBeNull(); expect(disabled.snapshot()).toEqual({ counters: {}, metrics: {} });
});
