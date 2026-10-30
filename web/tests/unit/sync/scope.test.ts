import { expect, test, vi } from 'vitest';
import { append, chat, event, harness } from './helpers';
import type { HubEvent } from '../../../src/lib/sync/wire';

test('scope_accepts_only_contiguous_events_and_drops_duplicates', async () => {
  const h = await harness();
  h.sync.onFrame({ op: 'event', id: 'old', event: event('1') });
  for (const n of ['1', '1', '2', '1']) h.sync.onFrame({ op: 'event', id: h.id, event: event(n) });
  h.sync.onFrame({ op: 'event', id: h.id, event: event('5', '6') });
  await h.clock.advanceAsync(32);
  expect(h.accepted.map(e => e.event.scope_seq)).toEqual(['1', '2']); expect(h.sync.cursor).toBe('2');
  expect(h.probe.snapshot()).toMatchObject({ counters: { invalid_range: 1 } }); h.dispose();
});
test('partial_overlap_range_is_applied_once_and_flagged', async () => {
  const h = await harness();
  h.sync.onFrame({ op: 'event', id: h.id, event: event('105', '1') });
  const merged = event('110', '103');
  h.sync.onFrame({ op: 'event', id: h.id, event: merged }); h.sync.onFrame({ op: 'event', id: h.id, event: merged });
  await h.clock.advanceAsync(32);
  expect(h.accepted.at(-1)).toEqual({ event: merged, overlap: true }); expect(h.accepted).toHaveLength(2);
  expect(h.probe.ledger.verify(chat, '110').ok).toBe(true); h.dispose();
});
test('gap_buffers_live_events_fetches_the_range_and_drains_in_order', async () => {
  const h = await harness();
  for (let n = 1; n <= 600; n++) append(h.hub, n, n % 10 === 0);
  h.sync.onFrame({ op: 'event', id: h.id, event: h.hub.log(chat)[599]! });
  await h.clock.advanceAsync(160);
  expect(h.applier.fetchRange).toHaveBeenCalledWith('0', 256);
  expect(vi.mocked(h.applier.fetchRange).mock.calls.length).toBe(3);
  expect(h.accepted.map(e => e.event.scope_seq)).toEqual(h.hub.log(chat).filter(e => e.kind !== 'hidden').map(e => e.scope_seq));
  expect(h.sync.cursor).toBe('600'); expect(h.probe.ledger.verify(chat, '600').ok).toBe(true); h.dispose();
});
test.each(['count', 'bytes'])('gap_buffer_overflow_falls_back_to_resync_%s', async mode => {
  const h = await harness();
  vi.mocked(h.applier.fetchRange).mockImplementation(() => new Promise(() => {}));
  if (mode === 'count') for (let n = 10; n < 5011; n++) h.sync.onFrame({ op: 'event', id: h.id, event: event(String(n)) });
  else { const e = event('9'); if (e.kind === 'block_delta') e.payload.append = 'x'.repeat(8 * 1024 * 1024); h.sync.onFrame({ op: 'event', id: h.id, event: e }); }
  await h.clock.advanceAsync(0);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2); expect(h.sync.status).toBe('live'); h.dispose();
});
test('range_fetch_returning_resync_reloads_the_snapshot_once', async () => {
  const h = await harness(); vi.mocked(h.applier.fetchRange).mockResolvedValue({ resync: true });
  h.sync.onFrame({ op: 'event', id: h.id, event: event('3') }); await h.clock.advanceAsync(0);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2); expect(h.wire.sent.filter(op => op.op === 'subscribe')).toHaveLength(2); h.dispose();
});
test('resync_required_overrides_a_pending_range_retry', async () => {
  const h = await harness(); vi.mocked(h.applier.fetchRange).mockRejectedValueOnce(new Error('network_down'));
  append(h.hub, 1); append(h.hub, 2);
  h.sync.onFrame({ op: 'event', id: h.id, event: h.hub.log(chat)[1]! }); await h.clock.advanceAsync(0);
  expect(h.sync.status).toBe('gap_recovery'); h.hub.chaos.forceResync('cursor_too_old'); await h.clock.advanceAsync(0);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2); expect(h.sync.status).toBe('live');
  append(h.hub, 3); h.hub.flush(); await h.clock.advanceAsync(32); expect(h.sync.cursor).toBe('3'); h.dispose();
});
test.each(['cursor_too_old', 'cursor_ahead'] as const)('resync_required_loads_one_snapshot_and_subscribes_from_the_boundary_without_a_loop_%s', async reason => {
  const h = await harness(); append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(16);
  h.hub.chaos.forceResync(reason); await h.clock.advanceAsync(0);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2);
  expect(h.wire.sent.slice(-2)).toEqual([{ op: 'unsubscribe', id: expect.any(String) }, { op: 'subscribe', id: h.id, scope: chat, after: '1' }]);
  h.hub.chaos.forceResync(reason); expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2);
  await h.clock.advanceAsync(400); expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(3);
  h.hub.chaos.forceResync(reason); expect(h.sync.status).toBe('degraded');
  await h.clock.advanceAsync(60_000); expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(4); h.dispose();
});
test.each(['cursor_too_old', 'cursor_ahead'] as const)('persistent_resync_is_bounded_without_reconnect_%s', async reason => {
  const h = await harness(); const send = h.wire.send.bind(h.wire);
  vi.spyOn(h.wire, 'send').mockImplementation(op => {
    const result = send(op);
    if (op.op === 'subscribe') h.hub.chaos.forceResync(reason);
    return result;
  });
  h.hub.chaos.forceResync(reason); await h.clock.advanceAsync(59_999);
  expect(vi.mocked(h.applier.loadSnapshot).mock.calls.length).toBeLessThanOrEqual(4);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(4); expect(h.sync.status).toBe('degraded');
  expect(h.wire.opens).toBe(1); expect(h.applier.fetchRange).not.toHaveBeenCalled();
  expect(h.wire.sent.filter(op => op.op === 'subscribe').every(op => op.op === 'subscribe' && op.after === '0')).toBe(true); h.dispose();
});
test('dropped_frames_on_a_live_socket_are_detected_as_a_gap', async () => {
  const h = await harness(); h.hub.chaos.dropNext(1); append(h.hub, 1); h.hub.flush(); append(h.hub, 2); h.hub.flush();
  await h.clock.advanceAsync(32); expect(h.applier.fetchRange).toHaveBeenCalled(); expect(h.sync.cursor).toBe('2');
  expect(h.probe.ledger.verify(chat, '2').ok).toBe(true); h.dispose();
});
test('reordered_frames_are_buffered_until_contiguous', async () => {
  const h = await harness(); h.hub.chaos.reorderNext(); append(h.hub, 1); h.hub.flush(); append(h.hub, 2); h.hub.flush();
  await h.clock.advanceAsync(32); expect(h.accepted.map(e => e.event.scope_seq)).toEqual(['1', '2']); h.dispose();
});
test('hidden_placeholder_moves_the_cursor_and_is_applied_by_no_reducer', async () => {
  const h = await harness(); append(h.hub, 1, true); append(h.hub, 2, true); h.hub.flush(); await h.clock.advanceAsync(16);
  expect(h.sync.cursor).toBe('2'); expect(h.applier.applyEvents).not.toHaveBeenCalled();
  expect(h.probe.ledger.verify(chat, '2').ok).toBe(true); h.dispose();
});
test('incompatible_event_version_stops_in_the_incompatible_state', async () => {
  const h = await harness(); const e = { ...event('1'), v: 2 } as unknown as HubEvent;
  h.sync.onFrame({ op: 'event', id: h.id, event: e }); expect(h.sync.status).toBe('incompatible');
  h.sync.onTransportClosed(); h.sync.onTransportOpen(); expect(h.sync.status).toBe('incompatible'); expect(h.sync.cursor).toBe('0'); h.dispose();
});
test('forbidden_scope_is_terminal_and_not_retried', async () => {
  const h = await harness(); h.sync.onFrame({ op: 'error', id: h.id, code: 'forbidden' });
  h.sync.onTransportClosed(); h.sync.onTransportOpen(); await h.clock.advanceAsync(120_000);
  expect(h.sync.status).toBe('forbidden'); expect(h.wire.sent.filter(op => op.op === 'subscribe')).toHaveLength(1); h.dispose();
});
test('too_many_subscriptions_error_is_surfaced_without_reconnecting', async () => {
  const h = await harness(); h.sync.onFrame({ op: 'error', id: h.id, code: 'too_many_subscriptions' });
  await h.clock.advanceAsync(60_000); expect(h.sync.status).toBe('degraded'); expect(h.wire.opens).toBe(1); h.dispose();
});
test('merged_event_age_uses_committed_at_from_of_the_oldest_source', async () => {
  const h = await harness(); await h.clock.advanceAsync(100);
  h.sync.onFrame({ op: 'event', id: h.id, event: { ...event('2', '1'), committed_at: new Date(90).toISOString() } });
  expect(h.probe.snapshot()).toMatchObject({ metrics: { event_age_ms: [100] } }); h.dispose();
});
test('cursor_ahead_snapshot_rebases_the_ledger_before_accepting_a_new_tail', async () => {
  const h = await harness();
  h.sync.onFrame({ op: 'event', id: h.id, event: event('2', '1') });
  h.hub.chaos.forceResync('cursor_ahead'); await h.clock.advanceAsync(0);
  expect(h.sync.cursor).toBe('0'); expect(h.probe.ledger.verify(chat, '0').ok).toBe(true);
  append(h.hub, 1); h.hub.flush(); await h.clock.advanceAsync(32);
  expect(h.probe.ledger.verify(chat, '1')).toEqual({ ok: true, holes: [], doubled: [] }); h.dispose();
});
test('oversized_is_refetched_before_advancing_and_retention_resyncs', async () => {
  for (const resync of [false, true]) {
    const h = await harness(); const original = append(h.hub, 1);
    if (resync) vi.mocked(h.applier.fetchRange).mockResolvedValue({ resync: true });
    h.sync.onFrame({ op: 'event', id: h.id, event: { ...original, kind: 'oversized', payload: { kind: original.kind, href: '/api/v1/events?scope=x&after=0&limit=1' } } });
    expect(h.sync.cursor).toBe('0'); await h.clock.advanceAsync(32);
    expect(h.applier.fetchRange).toHaveBeenCalledWith('0', 1); expect(h.sync.cursor).toBe('1');
    if (resync) expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(2);
    else expect(h.accepted[0]?.event).toEqual(original);
    h.dispose();
  }
});

test.each([
  ['conversation_deleted', 'deleted'],
  ['not_found', 'forbidden']
])('snapshot_refusal_%s_is_terminal_and_never_retried', async (code, status) => {
  const h = await harness();
  vi.mocked(h.applier.loadSnapshot).mockClear();
  vi.mocked(h.applier.loadSnapshot).mockRejectedValue(Object.assign(new Error('refused'), { code }));
  h.sync.onFrame({ op: 'resync_required', id: h.id, reason: 'cursor_too_old', oldest_available_seq: '1' });
  await h.clock.advanceAsync(120_000);
  expect(h.sync.status).toBe(status);
  expect(h.applier.loadSnapshot).toHaveBeenCalledTimes(1);
  h.dispose();
});
test('a_transient_snapshot_failure_is_still_retried', async () => {
  const h = await harness();
  vi.mocked(h.applier.loadSnapshot).mockClear();
  vi.mocked(h.applier.loadSnapshot).mockRejectedValueOnce(new Error('network'));
  h.sync.onFrame({ op: 'resync_required', id: h.id, reason: 'cursor_too_old', oldest_available_seq: '1' });
  await h.clock.advanceAsync(60_000);
  expect(vi.mocked(h.applier.loadSnapshot).mock.calls.length).toBeGreaterThan(1);
  h.dispose();
});
