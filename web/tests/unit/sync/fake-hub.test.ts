import { expect, test, vi } from 'vitest';
import { FakeClock, FakeHub, checkEvent, checkServer, mergeEvents } from '../../../testing/fake-hub';
import { BrowserSocket } from '../../../src/lib/sync/transport';
import type { ClientOp, HubEvent, ServerOp } from '../../../src/lib/sync/wire';
import { append, block, chat, event } from './helpers';

test('fake_hub_events_validate_against_the_published_event_schema', () => {
  const hub = new FakeHub(), clock = new FakeClock(), wire = hub.wire.memory(clock); const frames: ServerOp[] = [];
  hub.setSnapshot(chat, () => null);
  wire.on({ open: () => {}, close: () => {}, message: data => { const f = JSON.parse(data); checkServer(f); frames.push(f); } });
  wire.open(); clock.advance(0); wire.send({ op: 'subscribe', scope: chat, id: 's1', after: '0' });
  for (let n = 1; n <= 20; n++) { checkEvent(append(hub, n, n % 5 === 0)); hub.flush(); }
  expect(frames.filter(f => f.op === 'event')).toHaveLength(20);
  const hidden = hub.log(chat).find(e => e.kind === 'hidden'); expect(hidden).toMatchObject({ payload: {}, conversation_id: null, execution_id: null });
  expect(() => checkEvent({ ...event('1'), payload: {} })).toThrow('fake_hub_invalid_event'); wire.close();
});
test('fake_hub_merger_follows_the_1_3a_rules', () => {
  const first = event('1'), second = event('2');
  if (first.kind !== 'block_delta' || second.kind !== 'block_delta') throw new Error('wrong_fixture');
  first.payload = { block_id: block, base_rev: 0, rev: 1, append: 'а' };
  second.payload = { block_id: block, base_rev: 1, rev: 2, append: 'б' };
  second.id = '00000000-0000-4000-8000-000000000011';
  second.correlation_id = '00000000-0000-4000-8000-000000000012';
  second.committed_at = new Date(24).toISOString();
  second.committed_at_from = second.committed_at;
  const merged = mergeEvents([first, second], 's1');
  expect(merged).toHaveLength(1); expect(merged[0]).toMatchObject({ id: first.id, correlation_id: first.correlation_id, committed_at_from: first.committed_at,
    committed_at: second.committed_at, scope_seq_from: '1', scope_seq: '2', payload: { base_rev: 0, rev: 2, append: 'аб' } });
  for (const changed of [
    { ...second, committed_at: new Date(25).toISOString() }, { ...second, scope_seq: '3', scope_seq_from: '3' },
    { ...second, scope: 'machines' }, { ...second, execution_id: block },
    { ...second, conversation_id: block }, { ...second, payload: { ...second.payload, rev: 1 } },
    { ...second, payload: { ...second.payload, base_rev: 0 } }, { ...second, payload: { ...second.payload, block_id: 'another' } }
  ]) expect(mergeEvents([first, changed], 's1')).toHaveLength(2);
  const large = { ...second, payload: { ...second.payload, append: '\\'.repeat(33_000) } };
  expect(mergeEvents([first, large], 's1')).toHaveLength(2);
  const hidden = (n: string): HubEvent => ({ ...event(n), kind: 'hidden', payload: {}, conversation_id: null, execution_id: null });
  const terminal: HubEvent = { ...event('4'), kind: 'block_final', payload: { block_id: block, rev: 4, text: 'конец', checksum: '0'.repeat(64), text_omitted: false, summary: null } };
  const mixed = mergeEvents([first, hidden('2'), hidden('3'), terminal], 's1');
  expect(mixed.map(e => [e.kind, e.scope_seq_from, e.scope_seq])).toEqual([['block_delta', '1', '1'], ['hidden', '2', '3'], ['block_final', '4', '4']]);
  expect(mergeEvents([hidden('1'), { ...hidden('2'), committed_at: new Date(25).toISOString() }], 's1')).toHaveLength(2);
  expect(mergeEvents([first, { ...second, committed_at: new Date(60_000).toISOString() }], 's1', 25, 65_536, true, () => 0)).toHaveLength(1);
});
test('fake_hub_enforces_retention_replay_limits_lag_close_and_oversized', () => {
  const clock = new FakeClock(), hub = new FakeHub({ journalRetention: 2, maxFrameBytes: 1024 }), wire = hub.wire.memory(clock);
  const frames: ServerOp[] = [], closes: number[] = [];
  wire.on({ open: () => {}, close: i => { closes.push(i.code); }, message: data => { frames.push(JSON.parse(data)); } });
  for (let n = 1; n <= 3; n++) append(hub, n);
  wire.open(); clock.advance(0);
  wire.send({ op: 'subscribe', id: 'internal', scope: `owner:${block}:conversations` });
  expect(frames.at(-1)).toMatchObject({ op: 'error', code: 'bad_scope' });
  wire.send({ op: 'subscribe', id: 'foreign', scope: `conversation:${block}` });
  expect(frames.at(-1)).toMatchObject({ op: 'error', code: 'forbidden' });
  wire.send({ op: 'subscribe', id: 'old', scope: chat, after: '0' }); wire.send({ op: 'subscribe', id: 'ahead', scope: chat, after: '4' });
  expect(frames.slice(-2)).toMatchObject([{ op: 'resync_required', reason: 'cursor_too_old' }, { op: 'resync_required', reason: 'cursor_ahead' }]);
  wire.send({ op: 'subscribe', id: 's', scope: chat, after: '1' }); expect(frames.at(-1)).toMatchObject({ op: 'event', event: { scope_seq: '3', scope_seq_from: '2' } });
  const e = event('4'); if (e.kind !== 'block_delta') throw new Error('wrong_fixture');
  hub.append(chat, { kind: 'block_delta', payload: { ...e.payload, append: 'я'.repeat(1000) } }); hub.flush();
  expect(frames.at(-1)).toMatchObject({ op: 'event', event: { kind: 'oversized', scope_seq: '4', payload: { href: expect.stringContaining('after=3&limit=1') } } });
  expect(hub.range(chat, '3', 1)).toMatchObject({ events: [{ kind: 'block_delta' }] });
  for (let n = 0; n < 16; n++) wire.send({ op: 'subscribe', id: `m${n}`, scope: 'machines' });
  expect(frames.at(-1)).toMatchObject({ op: 'error', code: 'too_many_subscriptions' }); expect(closes).toEqual([]);
  hub.chaos.lag(chat); expect(frames.at(-1)?.op).toBe('lagged'); expect(closes).toEqual([1008]);
  expect(clock.pending).toBe(0);
});
test('fake_hub_delay_and_seeded_clock_are_deterministic', () => {
  const a = new FakeClock(42), b = new FakeClock(42); expect([a.rng(), a.rng()]).toEqual([b.rng(), b.rng()]);
  const hub = new FakeHub(), wire = hub.wire.memory(a), receive = vi.fn();
  wire.on({ open: () => {}, close: () => {}, message: receive }); wire.open(); a.advance(0); hub.chaos.delay(100);
  wire.send({ op: 'ping', t: 1 }); a.advance(99); expect(receive).not.toHaveBeenCalled(); a.advance(1); expect(receive).toHaveBeenCalledOnce();
  wire.send({ op: 'ping', t: 2 }); wire.close(); expect(a.pending).toBe(0);
});
test('browser_socket_uses_the_subprotocol_and_reports_close_codes', () => {
  class Socket {
    static OPEN = 1; readyState = 0; static instances: Socket[] = [];
    onopen: (() => void) | null = null; onmessage: ((e: { data: string }) => void) | null = null;
    onclose: ((e: { code: number; reason: string }) => void) | null = null; onerror: (() => void) | null = null;
    send = vi.fn(); close = vi.fn();
    constructor(readonly url: string, readonly protocol: string) { Socket.instances.push(this); }
  }
  vi.stubGlobal('WebSocket', Socket);
  try {
    const wire = new BrowserSocket(() => 'wss://example.test/api/v1/ws'), open = vi.fn(), close = vi.fn(), message = vi.fn();
    const off = wire.on({ open, close, message }); wire.open(); const socket = Socket.instances[0]!;
    expect(socket.protocol).toBe('canterlot.v1'); expect(socket.url).toBe('wss://example.test/api/v1/ws');
    expect(wire.send({ op: 'ping', t: 1 })).toBe(false); socket.readyState = 1; socket.onopen?.(); expect(open).toHaveBeenCalledOnce();
    wire.send({ op: 'ping', t: 1 }); expect(socket.send).toHaveBeenCalledWith('{"op":"ping","t":1}');
    socket.onmessage?.({ data: 'frame' }); expect(message).toHaveBeenCalledWith('frame');
    socket.onclose?.({ code: 4401, reason: 'reauth' }); expect(close).toHaveBeenCalledWith({ code: 4401, reason: 'reauth', opened: true });
    wire.open(); const second = Socket.instances[1]!; second.readyState = 1; second.onopen?.(); second.readyState = 2;
    wire.close(); expect(close).toHaveBeenLastCalledWith({ code: 1000, reason: '', opened: true });
    off(); expect(second.close).toHaveBeenCalled();
    const malformed = { op: 'subscribe', id: 'x', scope: chat, after: 1 } as unknown as ClientOp;
    expect(wire.send(malformed)).toBe(false);
  } finally { vi.unstubAllGlobals(); }
});
