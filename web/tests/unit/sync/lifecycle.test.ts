import { expect, test, vi } from 'vitest';
import { browserLifecycle } from '../../../src/lib/sync/lifecycle';
import { FakeClock } from '../../../testing/fake-hub';
import { connectionHarness } from './helpers';

function lifecycleHarness() {
  const clock = new FakeClock(); let wallOffset = 0; let visible = true;
  const doc = new EventTarget() as Document;
  Object.defineProperty(doc, 'visibilityState', { get: () => visible ? 'visible' : 'hidden' });
  const win = new EventTarget() as Window;
  const addDoc = vi.spyOn(doc, 'addEventListener'), removeDoc = vi.spyOn(doc, 'removeEventListener');
  const addWin = vi.spyOn(win, 'addEventListener'), removeWin = vi.spyOn(win, 'removeEventListener');
  const lifecycle = browserLifecycle({ window: win, document: doc, wall: () => clock.now() + wallOffset, monotonic: clock.now, schedule: clock.schedule });
  return { clock, doc, win, lifecycle, addDoc, removeDoc, addWin, removeWin,
    jump: (ms: number) => { wallOffset += ms; }, visibility: (value: boolean) => { visible = value; doc.dispatchEvent(new Event('visibilitychange')); } };
}
test('wall_clock_jump_triggers_probe', async () => {
  const h = lifecycleHarness(), resume = vi.fn(), off = h.lifecycle.onResume(resume);
  h.jump(40 * 60_000); h.clock.advance(1000); expect(resume).toHaveBeenCalledWith({ hiddenMs: 0, reason: 'clock_jump' });
  const c = await connectionHarness(); c.lifecycle.resume('clock_jump', 0);
  expect(c.wire.sent.at(-1)?.op).toBe('ping'); c.manager.dispose(); off(); expect(h.clock.pending).toBe(0);
});
test('bfcache_and_freeze_resume_trigger_probe', async () => {
  const h = lifecycleHarness(), resume = vi.fn(), suspend = vi.fn();
  const off = h.lifecycle.onResume(resume), offSuspend = h.lifecycle.onSuspend(suspend);
  const persisted = new Event('pageshow'); Object.defineProperty(persisted, 'persisted', { value: true }); h.win.dispatchEvent(persisted);
  h.doc.dispatchEvent(new Event('freeze')); h.clock.advance(10); h.doc.dispatchEvent(new Event('resume'));
  expect(resume.mock.calls.map(call => call[0].reason)).toEqual(['pageshow', 'resume']); expect(suspend).toHaveBeenCalledWith('freeze');
  const c = await connectionHarness(); const opens = c.wire.opens;
  c.lifecycle.resume('pageshow', 0); c.lifecycle.resume('resume', 0);
  expect(c.wire.sent.filter(op => op.op === 'ping')).toHaveLength(2); expect(c.wire.opens).toBe(opens); c.manager.dispose();
  off(); offSuspend(); expect(h.clock.pending).toBe(0);
  expect(h.addDoc.mock.calls.length + h.addWin.mock.calls.length).toBe(h.removeDoc.mock.calls.length + h.removeWin.mock.calls.length);
});
test('window_focus_resumes_the_connection', () => {
  const h = lifecycleHarness(), resume = vi.fn();
  const off = h.lifecycle.onResume(resume);
  h.win.dispatchEvent(new Event('focus'));
  expect(resume).toHaveBeenCalledWith({ reason: 'focus', hiddenMs: 0 });
  off(); expect(h.clock.pending).toBe(0);
  expect(h.addWin.mock.calls.length).toBe(h.removeWin.mock.calls.length);
});
test('lifecycle_tracks_hidden_time_online_offline_and_unsubscribes_every_listener', () => {
  const h = lifecycleHarness(), resume = vi.fn(), suspend = vi.fn();
  const off = h.lifecycle.onResume(resume), offSuspend = h.lifecycle.onSuspend(suspend);
  h.visibility(false); h.clock.advance(6000); h.visibility(true);
  expect(resume).toHaveBeenCalledWith({ reason: 'visible', hiddenMs: 6000 });
  h.win.dispatchEvent(new Event('offline')); h.win.dispatchEvent(new Event('online')); h.win.dispatchEvent(new Event('pagehide'));
  expect(resume).toHaveBeenLastCalledWith({ reason: 'online', hiddenMs: 0 }); expect(suspend).toHaveBeenLastCalledWith('pagehide');
  off(); offSuspend(); expect(h.clock.pending).toBe(0);
  expect(h.addDoc.mock.calls.length + h.addWin.mock.calls.length).toBe(h.removeDoc.mock.calls.length + h.removeWin.mock.calls.length);
});
