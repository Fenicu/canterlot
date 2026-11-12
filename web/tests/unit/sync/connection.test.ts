import { expect, test, vi } from 'vitest';
import { append, chat, connectionHarness, event, TestLifecycle } from './helpers';
import { backoff } from '../../../src/lib/sync/backoff';
import { ConnectionManager } from '../../../src/lib/sync/connection';
import { FakeClock, FakeHub } from '../../../testing/fake-hub';
import type { TransportHandlers } from '../../../src/lib/sync/transport';
import type { ClientOp } from '../../../src/lib/sync/wire';

test('reconnect_resubscribes_from_last_applied_cursor_and_loses_nothing', async () => {
  const h = await connectionHarness(); append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(16);
  h.hub.chaos.cut(); for (let n = 2; n <= 40; n++) append(h.hub, n);
  await h.clock.advanceAsync(1000);
  expect(h.wire.sent.filter(op => op.op === 'subscribe').at(-1)).toMatchObject({ after: '1' });
  expect(h.accepted.map(e => e.event.kind === 'block_delta' ? e.event.payload.append : '').join('')).toBe(h.hub.log(chat).map(e => e.kind === 'block_delta' ? e.payload.append : '').join(''));
  expect(h.subscription.cursor).toBe('40'); h.manager.dispose();
});
test('lagged_resubscribes_that_scope_from_the_own_cursor_after_the_connection_closes', async () => {
  const h = await connectionHarness(); const machines = h.manager.subscribe('machines', { ...h.applier,
    loadSnapshot: async () => ({ cursor: '0', data: null }), fetchRange: async () => ({ events: [], head: '0' }) });
  await h.clock.advanceAsync(0);
  h.hub.chaos.dropNext(1); append(h.hub, 1); h.hub.flush(); h.hub.chaos.lag(chat);
  await h.clock.advanceAsync(1000);
  expect(h.wire.sent.filter(op => op.op === 'subscribe' && op.scope === chat).at(-1)).toMatchObject({ after: '0' });
  expect(h.subscription.cursor).toBe('1'); expect(machines.status).toBe('live'); expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(1); h.manager.dispose();
});
test('second_lagged_within_60_seconds_switches_the_scope_to_resync', async () => {
  const h = await connectionHarness(); h.hub.chaos.lag(chat); await h.clock.advanceAsync(1000);
  h.hub.chaos.lag(chat); await h.clock.advanceAsync(1000);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2); expect(h.manager.status.resyncs).toBe(1); h.manager.dispose();
});
test('close_4401_reconnects_immediately_without_backoff_and_keeps_cursors', async () => {
  const h = await connectionHarness(); append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(16);
  h.hub.chaos.close4401(); await h.clock.advanceAsync(0);
  expect(h.wire.opens).toBe(2); expect(h.subscription.cursor).toBe('1'); expect(h.manager.status.attempt).toBe(0); h.manager.dispose();
});
test('silent_dead_socket_after_resume_is_detected_by_ping_deadline_and_reconnected_from_cursor', async () => {
  const h = await connectionHarness(); append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(16);
  const other = h.manager.subscribe('machines', { ...h.applier, loadSnapshot: async () => ({ data: null, cursor: '0' }) });
  await h.clock.advanceAsync(0); h.lifecycle.suspend(); h.hub.chaos.blackhole(true);
  for (let n = 2; n <= 41; n++) { append(h.hub, n); h.hub.flush(); }
  await h.clock.advanceAsync(600_000); expect(h.wire.opens).toBe(1);
  h.lifecycle.resume('visible', 600_000); await h.clock.advanceAsync(1999); expect(h.wire.opens).toBe(1);
  await h.clock.advanceAsync(100);
  expect(h.wire.opens).toBe(2); expect(h.wire.sent.filter(op => op.op === 'subscribe' && op.scope === chat).at(-1)).toMatchObject({ after: '1' });
  expect(other.status).toBe('live'); expect(h.subscription.cursor).toBe('41');
  expect(h.accepted.filter(e => e.event.kind === 'block_delta').map(e => e.event.kind === 'block_delta' ? e.event.payload.append : '').join('')).toBe(h.hub.log(chat).map(e => e.kind === 'block_delta' ? e.payload.append : '').join(''));
  h.manager.dispose();
});
test('online_event_triggers_an_immediate_reconnect_attempt', async () => {
  const h = await connectionHarness(); h.hub.chaos.cut(); h.lifecycle.resume('online', 0); await h.clock.advanceAsync(0);
  expect(h.wire.opens).toBe(2); h.manager.dispose();
});
test('resume_replaces_a_socket_stuck_before_open', async () => {
  const clock = new FakeClock(), hub = new FakeHub(), wire = hub.wire.memory(clock), lifecycle = new TestLifecycle();
  const manager = new ConnectionManager({ transport: wire, lifecycle, now: clock.now, rng: clock.rng, schedule: clock.schedule,
    raf: clock.raf, cancelRaf: clock.cancelRaf, probeAuth: async () => 'ok', announce: vi.fn() });
  const subscription = manager.subscribe(chat, {
    loadSnapshot: async () => ({ data: null, cursor: hub.snapshot(chat).cursor }), applySnapshot: vi.fn(), applyEvents: vi.fn(),
    fetchRange: async (after, limit) => hub.range(chat, after, limit)
  });
  lifecycle.resume('online', 0); await clock.advanceAsync(0);
  expect(wire.opens).toBe(2); expect(manager.status.phase).toBe('live'); expect(subscription.status).toBe('live');
  manager.dispose(); expect(clock.pending).toBe(0);
});
test('ping_runs_every_25s_while_visible_and_a_missing_pong_forces_reconnect', async () => {
  const h = await connectionHarness(); await h.clock.advanceAsync(25_000);
  expect(h.wire.sent.filter(op => op.op === 'ping')).toHaveLength(1);
  h.lifecycle.suspend(); await h.clock.advanceAsync(50_000); expect(h.wire.sent.filter(op => op.op === 'ping')).toHaveLength(1);
  h.hub.chaos.blackhole(true); h.lifecycle.resume(); await h.clock.advanceAsync(2000);
  expect(h.wire.opens).toBe(2);
  h.hub.chaos.blackhole(true); await h.clock.advanceAsync(25_000); const opens = h.wire.opens;
  await h.clock.advanceAsync(4999); expect(h.wire.opens).toBe(opens); await h.clock.advanceAsync(1);
  expect(h.manager.status.phase).toBe('reconnecting'); await h.clock.advanceAsync(325); expect(h.wire.opens).toBe(opens + 1); h.manager.dispose();
});
test('backoff_is_exponential_with_jitter_capped_at_15s_and_resets_after_30s_stable', async () => {
  expect([0, 1, 2, 3].map(n => backoff(n, () => 0.5))).toEqual([250, 500, 1000, 2000]);
  expect(backoff(0, () => 0)).toBe(175); expect(backoff(0, () => 1)).toBe(325); expect(backoff(100, () => 1)).toBe(15_000);
  const h = await connectionHarness(); h.hub.chaos.cut(); await h.clock.advanceAsync(1000);
  h.hub.chaos.cut(); await h.clock.advanceAsync(1000); expect(h.manager.status.attempt).toBe(2);
  await h.clock.advanceAsync(30_000); expect(h.manager.status.attempt).toBe(0); h.manager.dispose();
});
test.each(['expired', 'down'] as const)('two_failed_connects_without_open_probe_authentication_%s', async result => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handler: TransportHandlers | undefined;
  const probeAuth = vi.fn(async () => result);
  const transport = { on: (h: TransportHandlers) => { handler = h; return () => { handler = undefined; }; }, send: () => false,
    open: vi.fn(() => { clock.schedule(() => handler?.close({ code: 1006, reason: '', opened: false }), 0); }), close: vi.fn() };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth, announce: vi.fn() });
  await clock.advanceAsync(400); expect(probeAuth).toHaveBeenCalledTimes(1);
  expect(manager.status.phase).toBe(result === 'expired' ? 'auth_required' : 'offline');
  if (result === 'expired') { await clock.advanceAsync(60_000); expect(transport.open).toHaveBeenCalledTimes(2); }
  manager.dispose(); expect(clock.pending).toBe(0);
});
test('connection_status_transitions_are_announced_once_each', async () => {
  const h = await connectionHarness(); for (let n = 1; n <= 20; n++) { append(h.hub, n); h.hub.flush(); }
  await h.clock.advanceAsync(100); expect(h.announcements).toEqual(['Подключение', 'Соединение установлено']);
  h.hub.chaos.cut(); await h.clock.advanceAsync(1000);
  expect(h.announcements).toEqual(['Подключение', 'Соединение установлено', 'Восстановление соединения', 'Соединение установлено']); h.manager.dispose();
});
test('a_quick_return_to_visible_probes_the_socket_immediately', async () => {
  const h = await connectionHarness(); append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(16);
  h.lifecycle.suspend(); h.hub.chaos.blackhole(true); await h.clock.advanceAsync(3000);
  h.lifecycle.resume('visible', 3000); await h.clock.advanceAsync(1999);
  expect(h.wire.opens).toBe(1); await h.clock.advanceAsync(100);
  expect(h.wire.opens).toBe(2); h.manager.dispose();
});
test('an_attempt_stuck_before_open_is_abandoned_and_retried', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    open: vi.fn(), send: () => false,
    close: vi.fn(() => { handlers?.close({ code: 1006, reason: '', opened: false }); })
  };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth: async () => 'ok', announce: vi.fn() });
  await clock.advanceAsync(9_999); expect(transport.open).toHaveBeenCalledTimes(1);
  await clock.advanceAsync(1000); expect(transport.open).toHaveBeenCalledTimes(2);
  expect(manager.status.phase).toBe('reconnecting'); expect(clock.pending).toBeGreaterThan(0);
  manager.dispose(); expect(clock.pending).toBe(0);
});
test('resume_that_replaces_a_stuck_attempt_does_not_count_as_an_auth_failure', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  const probeAuth = vi.fn(async () => 'ok' as const);
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    open: vi.fn(), send: () => false,
    close: vi.fn(() => { handlers?.close({ code: 1006, reason: '', opened: false }); })
  };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth, announce: vi.fn() });
  for (let n = 0; n < 4; n++) { await clock.advanceAsync(3000); manager.probe('online'); await clock.advanceAsync(0); }
  expect(probeAuth).not.toHaveBeenCalled(); expect(transport.open.mock.calls.length).toBeGreaterThan(4);
  manager.dispose();
});
test('burst_of_resume_events_does_not_restart_a_fresh_handshake', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    open: vi.fn(), send: () => false,
    close: vi.fn(() => { handlers?.close({ code: 1006, reason: '', opened: false }); })
  };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth: async () => 'ok', announce: vi.fn() });
  for (const reason of ['visible', 'focus', 'online', 'pageshow'] as const) { manager.probe(reason); await clock.advanceAsync(100); }
  expect(transport.open).toHaveBeenCalledTimes(2); expect(transport.close).toHaveBeenCalledTimes(1);
  manager.dispose();
});
test('auth_required_rechecks_auth_on_resume_and_recovers_without_reload', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  let result: 'ok' | 'expired' | 'down' = 'expired'; let opens = false;
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    send: () => true, open: vi.fn(() => { clock.schedule(() => { if (opens) handlers?.open(); else handlers?.close({ code: 1006, reason: '', opened: false }); }, 0); }),
    close: vi.fn(() => { handlers?.close({ code: 1000, reason: '', opened: true }); })
  };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth: async () => result, announce: vi.fn() });
  await clock.advanceAsync(1000); expect(manager.status.phase).toBe('auth_required');
  const attempts = transport.open.mock.calls.length;
  await clock.advanceAsync(120_000); expect(transport.open.mock.calls.length).toBe(attempts);
  result = 'ok'; opens = true; lifecycle.resume('online', 0); await clock.advanceAsync(0);
  expect(transport.open.mock.calls.length).toBe(attempts + 1); expect(manager.status.phase).toBe('live');
  manager.dispose(); expect(clock.pending).toBe(0);
});
test('auth_required_rechecks_only_when_the_session_is_still_expired', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    send: () => true, open: vi.fn(() => { clock.schedule(() => handlers?.close({ code: 1006, reason: '', opened: false }), 0); }),
    close: vi.fn(() => { handlers?.close({ code: 1000, reason: '', opened: true }); })
  };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth: async () => 'expired', announce: vi.fn() });
  await clock.advanceAsync(1000); const attempts = transport.open.mock.calls.length;
  expect(manager.status.phase).toBe('auth_required');
  lifecycle.resume('visible', 6000); await clock.advanceAsync(1000);
  expect(manager.status.phase).toBe('auth_required'); expect(transport.open.mock.calls.length).toBe(attempts);
  manager.dispose();
});
test('reconnect_never_gives_up_and_keeps_retrying_after_many_failures', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    send: () => false, open: vi.fn(() => { clock.schedule(() => handlers?.close({ code: 1006, reason: '', opened: false }), 0); }),
    close: vi.fn()
  };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth: async () => 'down', announce: vi.fn() });
  await clock.advanceAsync(10 * 60_000);
  expect(transport.open.mock.calls.length).toBeGreaterThan(30); expect(clock.pending).toBeGreaterThan(0);
  const before = transport.open.mock.calls.length; await clock.advanceAsync(30_000);
  expect(transport.open.mock.calls.length).toBeGreaterThan(before); manager.dispose();
});
test('incompatible_version_stops_every_scope_and_prompts_an_update', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(), sent: ClientOp[] = [], announce = vi.fn();
  let handlers: TransportHandlers | undefined;
  const transport = { on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    send: (op: ClientOp) => { sent.push(op); return true; }, open: vi.fn(() => { handlers?.open(); }),
    close: vi.fn(() => { handlers?.close({ code: 1000, reason: '', opened: true }); }) };
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, rng: clock.rng, schedule: clock.schedule, probeAuth: async () => 'ok', announce });
  const applier = { loadSnapshot: async () => ({ cursor: '0', data: null }), applySnapshot: vi.fn(), applyEvents: vi.fn(), fetchRange: async () => ({ events: [], head: '0' }) };
  const a = manager.subscribe(chat, applier), b = manager.subscribe('machines', applier); await clock.advanceAsync(0);
  const subscription = sent.find(op => op.op === 'subscribe' && op.scope === chat);
  if (subscription?.op !== 'subscribe') throw new Error('no_subscription');
  handlers?.message(JSON.stringify({ op: 'event', id: subscription.id, event: { ...event('1'), v: 2 } }));
  expect(manager.status.phase).toBe('incompatible'); expect(a.status).toBe('incompatible'); expect(b.status).toBe('incompatible');
  expect(announce).toHaveBeenLastCalledWith('Обновите приложение для продолжения');
  await clock.advanceAsync(60_000); expect(transport.open).toHaveBeenCalledTimes(1); expect(applier.applyEvents).not.toHaveBeenCalled(); manager.dispose(); expect(clock.pending).toBe(0);
});
test('release_frees_the_slot_after_the_grace_period', async () => {
  const h = await connectionHarness(); h.subscription.release(); await h.clock.advanceAsync(29_999);
  expect(h.wire.sent.filter(op => op.op === 'unsubscribe')).toHaveLength(0);
  const again = h.manager.subscribe(chat, h.applier); await h.clock.advanceAsync(2); expect(again.status).toBe('live');
  again.release(); await h.clock.advanceAsync(30_000); expect(h.wire.sent.filter(op => op.op === 'unsubscribe')).toHaveLength(1); h.manager.dispose();
});
test('dispose_closes_the_socket_and_cancels_all_timers', async () => {
  const h = await connectionHarness(); append(h.hub, 1); h.hub.flush(); h.hub.chaos.delay(1000); append(h.hub, 2); h.hub.flush();
  h.subscription.release(); h.lifecycle.suspend('pagehide'); h.manager.dispose();
  expect(h.accepted).toHaveLength(1); expect(h.wire.listeners).toBe(0); expect(h.clock.pending).toBe(0);
  expect(h.lifecycle.resumes.size + h.lifecycle.suspends.size).toBe(0); h.manager.dispose();
});
test('hub_restart_during_gap_recovery_resubscribes_and_resyncs_cleanly', async () => {
  const h = await connectionHarness(); let resolve: (() => void) | undefined;
  vi.mocked(h.applier.fetchRange).mockImplementationOnce(async (after, limit) => { await new Promise<void>(r => { resolve = r; }); return h.hub.range(chat, after, limit); });
  h.hub.chaos.dropNext(1); append(h.hub, 1); h.hub.flush(); append(h.hub, 2); h.hub.flush();
  expect(h.subscription.status).toBe('gap_recovery'); h.hub.chaos.restart();
  await h.clock.advanceAsync(1000); resolve?.(); await h.clock.advanceAsync(32);
  expect(h.subscription.cursor).toBe('2'); expect(h.accepted.map(e => e.event.kind === 'block_delta' ? e.event.payload.append : '').join('')).toBe('т1🙂т2🙂');
  h.hub.chaos.forceResync('cursor_too_old'); await h.clock.advanceAsync(0);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2); expect(h.subscription.status).toBe('live'); h.manager.dispose();
});
test('disconnect_during_range_retry_does_not_block_resubscription', async () => {
  const h = await connectionHarness();
  vi.mocked(h.applier.fetchRange).mockRejectedValueOnce(new Error('network_down'));
  h.hub.chaos.dropNext(1); append(h.hub, 1); h.hub.flush(); append(h.hub, 2); h.hub.flush();
  await h.clock.advanceAsync(0); expect(h.subscription.status).toBe('gap_recovery');
  h.hub.chaos.cut(); await h.clock.advanceAsync(1000);
  expect(h.subscription.status).toBe('live'); expect(h.subscription.cursor).toBe('2');
  expect(h.accepted.map(e => e.event.kind === 'block_delta' ? e.event.payload.append : '').join('')).toBe('т1🙂т2🙂'); h.manager.dispose();
});
test('released_scopes_never_hit_the_subscription_limit', async () => {
  const h = await connectionHarness();
  const applier = { ...h.applier, loadSnapshot: async () => ({ cursor: '0', data: null }), fetchRange: async () => ({ events: [], head: '0' }) };
  for (let n = 0; n < 40; n++) {
    const scope = `conversation:00000000-0000-4000-8000-${n.toString(16).padStart(12, '0')}` as const;
    const subscription = h.manager.subscribe(scope, { ...applier });
    await h.clock.advanceAsync(0);
    subscription.release();
  }
  const kept = h.manager.subscribe('machines', applier); await h.clock.advanceAsync(0);
  expect(kept.status).toBe('live'); h.manager.dispose();
});
test('released_scope_taken_by_a_new_applier_starts_a_fresh_sync_for_it', async () => {
  const h = await connectionHarness(); h.subscription.release(); await h.clock.advanceAsync(1000);
  const fresh = { ...h.applier, loadSnapshot: vi.fn(async () => { const s = h.hub.snapshot(chat); return { cursor: s.cursor, data: s.data }; }), applySnapshot: vi.fn(), applyEvents: vi.fn() };
  const again = h.manager.subscribe(chat, fresh); await h.clock.advanceAsync(2);
  expect(fresh.loadSnapshot).toHaveBeenCalledTimes(1);
  append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(16);
  expect(fresh.applyEvents).toHaveBeenCalled(); expect(h.accepted).toHaveLength(0);
  again.release(); h.manager.dispose();
});
test('close_revoked_stops_reconnecting_and_asks_the_runtime_why', async () => {
  const clock = new FakeClock(), lifecycle = new TestLifecycle(); let handlers: TransportHandlers | undefined;
  const transport = {
    on: (h: TransportHandlers) => { handlers = h; return () => { handlers = undefined; }; },
    send: () => true, open: vi.fn(() => { clock.schedule(() => handlers?.open(), 0); }),
    close: vi.fn(() => { handlers?.close({ code: 4401, reason: '', opened: true }); })
  };
  const revoked = vi.fn();
  const manager = new ConnectionManager({ transport, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng, probeAuth: async () => 'expired', revoked, announce: vi.fn() });
  await clock.advanceAsync(0); expect(manager.status.phase).toBe('live');
  handlers?.message(JSON.stringify({ op: 'close', reason: 'revoked' }));
  expect(manager.status.phase).toBe('auth_required'); expect(revoked).toHaveBeenCalledTimes(1);
  await clock.advanceAsync(120_000); expect(transport.open).toHaveBeenCalledTimes(1);
  manager.dispose();
});
