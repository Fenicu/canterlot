import { vi } from 'vitest';
import { FakeClock, FakeHub } from '../../../testing/fake-hub';
import { FrameBatcher } from '../../../src/lib/sync/batcher';
import { ScopeSync, type AppliedEvent, type ScopeApplier } from '../../../src/lib/sync/scope';
import { createProbe } from '../../../src/lib/perf/probe';
import type { HubEvent, ScopeName, ServerOp } from '../../../src/lib/sync/wire';
import type { Lifecycle, ResumeReason } from '../../../src/lib/sync/lifecycle';
import { ConnectionManager } from '../../../src/lib/sync/connection';

export const block = '00000000-0000-4000-8000-000000000010';
export const conversation = '00000000-0000-4000-8000-000000000020';
export const chat: ScopeName = `conversation:${conversation}`;
export function append(hub: FakeHub, rev: number, hidden = false, scope = chat): HubEvent {
  return hub.append(scope, hidden ? { kind: 'journal_closed', payload: { private: 'secret' } }
    : { kind: 'block_delta', conversation_id: conversation, payload: { block_id: block, base_rev: rev - 1, rev, append: `т${rev}🙂` } });
}
export function event(n: string, from = n): HubEvent {
  return { v: 1, id: block, correlation_id: block, scope: chat, scope_seq: n, scope_seq_from: from,
    committed_at: new Date(0).toISOString(), committed_at_from: new Date(0).toISOString(), kind: 'block_delta',
    conversation_id: conversation, execution_id: null, payload: { block_id: block, base_rev: 0, rev: 1, append: 'т' } };
}
export class TestLifecycle implements Lifecycle {
  visible = true;
  resumes = new Set<(i: { hiddenMs: number; reason: ResumeReason }) => void>();
  suspends = new Set<(r: 'hidden' | 'pagehide' | 'freeze') => void>();
  onResume(cb: (i: { hiddenMs: number; reason: ResumeReason }) => void) { this.resumes.add(cb); return () => { this.resumes.delete(cb); }; }
  onSuspend(cb: (r: 'hidden' | 'pagehide' | 'freeze') => void) { this.suspends.add(cb); return () => { this.suspends.delete(cb); }; }
  suspend(reason: 'hidden' | 'pagehide' | 'freeze' = 'hidden') { this.visible = false; for (const cb of this.suspends) cb(reason); }
  resume(reason: ResumeReason = 'visible', hiddenMs = 6000) { this.visible = true; for (const cb of this.resumes) cb({ hiddenMs, reason }); }
}
export async function harness(seed = 1, scope: ScopeName = chat) {
  const clock = new FakeClock(seed), hub = new FakeHub(), wire = hub.wire.memory(clock), lifecycle = new TestLifecycle();
  const accepted: AppliedEvent[] = [];
  const probe = createProbe(true);
  let snapshotCursor = '0';
  const applier: ScopeApplier = {
    loadSnapshot: vi.fn(async () => { const s = hub.snapshot(scope); return { cursor: s.cursor, data: s.data }; }),
    applySnapshot: vi.fn((_data, cursor) => { snapshotCursor = cursor; }),
    applyEvents: vi.fn(batch => { accepted.push(...batch); }),
    fetchRange: vi.fn(async (after, limit) => hub.range(scope, after, limit))
  };
  const frames: ServerOp[] = [];
  const batcher = new FrameBatcher<AppliedEvent>(batch => applier.applyEvents(batch), {
    raf: clock.raf, cancelRaf: clock.cancelRaf, now: clock.now, schedule: clock.schedule, visible: () => lifecycle.visible
  });
  const sync = new ScopeSync(scope, applier, { send: op => wire.send(op), now: clock.now, schedule: clock.schedule, rng: clock.rng, probe, batcher });
  const release = wire.on({ open: () => sync.onTransportOpen(), close: () => sync.onTransportClosed(),
    message: text => { const op = JSON.parse(text) as ServerOp; frames.push(op); sync.onFrame(op); } });
  sync.start(); wire.open(); await clock.advanceAsync(0);
  const dispose = () => { sync.stop(); release(); wire.close(); };
  return { clock, hub, wire, lifecycle, sync, applier, accepted, probe, frames, dispose,
    get snapshotCursor() { return snapshotCursor; },
    get id() { const op = wire.sent.findLast(op => op.op === 'subscribe'); if (op?.op !== 'subscribe') throw new Error('no_subscription'); return op.id; } };
}
export async function connectionHarness(seed = 1) {
  const clock = new FakeClock(seed), hub = new FakeHub(), wire = hub.wire.memory(clock), lifecycle = new TestLifecycle();
  const accepted: AppliedEvent[] = [];
  const announcements: string[] = [];
  const applier: ScopeApplier = {
    loadSnapshot: vi.fn(async () => { const s = hub.snapshot(chat); return { cursor: s.cursor, data: s.data }; }),
    applySnapshot: vi.fn(), applyEvents: vi.fn(batch => { accepted.push(...batch); }),
    fetchRange: vi.fn(async (after, limit) => hub.range(chat, after, limit))
  };
  const manager = new ConnectionManager({ transport: wire, lifecycle, now: clock.now, schedule: clock.schedule, rng: clock.rng,
    raf: clock.raf, cancelRaf: clock.cancelRaf, probeAuth: async () => 'ok', announce: text => { announcements.push(text); } });
  const subscription = manager.subscribe(chat, applier);
  await clock.advanceAsync(0);
  return { clock, hub, wire, lifecycle, manager, subscription, applier, accepted, announcements };
}
