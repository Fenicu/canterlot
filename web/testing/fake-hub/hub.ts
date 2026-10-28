import type { Page, Route } from '@playwright/test';
import { seq, type ClientOp, type HubEvent, type ServerOp } from '../../src/lib/sync/wire';
import type { RangeResult } from '../../src/lib/sync/scope';
import { Chaos } from './chaos';
import { mergeEvents } from './merger';
import { checkClient, checkEvent, checkServer, clientKinds } from './schema-check';
import type { FakeClock, HubClock, HubPeer, HubSession, ScopeJournal } from './state';
import { MemoryWire } from './wire-memory';
import { playwrightWire } from './wire-playwright';

const realClock: HubClock = {
  now: () => Date.now(), schedule: (fn, ms) => { const id = setTimeout(fn, ms); return () => clearTimeout(id); }
};
export class FakeHub {
  private scopes = new Map<string, ScopeJournal>();
  private sessions = new Set<HubSession>();
  private clock: HubClock = realClock;
  private uuid = 0n;
  private cancelFlush?: () => void;
  readonly chaos: Chaos;
  readonly routes = new Map<string, (route: Route) => Promise<void>>();
  readonly wire: { memory(clock: FakeClock): MemoryWire; playwright(page: Page): Promise<void> };
  constructor(private opts: { mergeWindowMs?: number; maxSubscriptions?: number; journalRetention?: number; maxFrameBytes?: number } = {}) {
    this.chaos = new Chaos(this.sessions, (s, code) => this.disconnect(s, code), scope => this.lag(scope));
    this.wire = {
      memory: clock => { this.clock = clock; return new MemoryWire(this, clock); },
      playwright: page => playwrightWire(this, page)
    };
  }
  private journal(scope: string): ScopeJournal {
    let state = this.scopes.get(scope);
    if (!state) { state = { head: '0', events: [], build: () => null }; this.scopes.set(scope, state); }
    return state;
  }
  private id(): string { return `00000000-0000-4000-8000-${(++this.uuid).toString(16).padStart(12, '0')}`; }
  append(scope: string, ev: { kind: string; payload: unknown; conversation_id?: string; execution_id?: string }): HubEvent {
    const state = this.journal(scope); state.head = seq.inc(state.head);
    const hidden = !clientKinds.has(ev.kind);
    const time = new Date(this.clock.now()).toISOString();
    const event = {
      v: 1, id: this.id(), correlation_id: this.id(), scope, scope_seq: state.head, scope_seq_from: state.head,
      committed_at: time, committed_at_from: time, kind: hidden ? 'hidden' : ev.kind,
      payload: hidden ? {} : structuredClone(ev.payload), conversation_id: hidden ? null : ev.conversation_id ?? null,
      execution_id: hidden ? null : ev.execution_id ?? null
    } as HubEvent;
    checkEvent(event); state.events.push(event);
    if (this.opts.journalRetention !== undefined) state.events.splice(0, Math.max(0, state.events.length - this.opts.journalRetention));
    if (!this.cancelFlush) this.cancelFlush = this.clock.schedule(() => { this.cancelFlush = undefined; this.flush(); }, Math.min(25, this.opts.mergeWindowMs ?? 25));
    return structuredClone(event);
  }
  head(scope: string): string { return this.journal(scope).head; }
  log(scope: string): HubEvent[] { return structuredClone(this.journal(scope).events); }
  range(scope: string, after: string, limit: number): RangeResult {
    const state = this.journal(scope);
    const oldest = state.events[0]?.scope_seq ?? seq.inc(state.head);
    if (seq.cmp(after, state.head) > 0 || BigInt(after) + 1n < BigInt(oldest)) return { resync: true };
    const events = state.events.filter(e => seq.cmp(e.scope_seq, after) > 0).slice(0, limit);
    // HTTP объединяет только скрытые заглушки.
    return { events: mergeEvents(events, '', 0, 65_536, false), head: state.head };
  }
  setSnapshot(scope: string, build: () => unknown): void { this.journal(scope).build = build; }
  snapshot(scope: string): { scope: string; cursor: string; data: unknown } {
    const state = this.journal(scope); const cursor = state.head; const data = structuredClone(state.build());
    return { scope, cursor, data };
  }
  attach(peer: HubPeer): HubSession {
    const session: HubSession = { peer, subscriptions: new Map(), closed: false, blocked: false, pending: new Set() };
    this.sessions.add(session); return session;
  }
  detach(session: HubSession): void {
    session.closed = true; session.subscriptions.clear(); session.held = undefined;
    for (const cancel of session.pending) cancel(); session.pending.clear(); this.sessions.delete(session);
    if (!this.sessions.size) { this.cancelFlush?.(); this.cancelFlush = undefined; }
  }
  private disconnect(session: HubSession, code: number): void {
    if (session.closed) return;
    this.detach(session); session.peer.close(code);
  }
  private emit(session: HubSession, op: ServerOp): void { checkServer(op); this.chaos.deliver(session, op); }
  receive(session: HubSession, op: ClientOp): void {
    if (session.closed || session.blocked) return;
    if (op?.op === 'subscribe' && typeof op.scope === 'string' && typeof op.id === 'string'
      && !/^(conversations|machines|conversation:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/.test(op.scope)) {
      this.emit(session, { op: 'error', id: op.id.slice(0, 128), code: 'bad_scope' }); return;
    }
    if (!checkClient(op)) {
      const id = op && typeof op === 'object' && 'id' in op && typeof op.id === 'string' && op.id.length <= 128 ? op.id : '';
      this.emit(session, { op: 'error', id, code: 'bad_message' }); return;
    }
    if (op.op === 'ping') { this.emit(session, { op: 'pong', t: op.t }); return; }
    if (op.op === 'unsubscribe') { session.subscriptions.delete(op.id); this.emit(session, { op: 'unsubscribed', id: op.id }); return; }
    if (op.after !== undefined && BigInt(op.after) > 9_223_372_036_854_775_807n) { this.emit(session, { op: 'error', id: op.id, code: 'bad_message' }); return; }
    if (session.subscriptions.has(op.id)) { this.emit(session, { op: 'error', id: op.id, code: 'bad_message' }); return; }
    if (session.subscriptions.size >= Math.min(16, this.opts.maxSubscriptions ?? 16)) { this.emit(session, { op: 'error', id: op.id, code: 'too_many_subscriptions' }); return; }
    if (op.scope.startsWith('conversation:') && !this.scopes.has(op.scope)) { this.emit(session, { op: 'error', id: op.id, code: 'forbidden' }); return; }
    const state = this.journal(op.scope), after = op.after ?? state.head;
    const range = this.range(op.scope, after, 256);
    if ('resync' in range || this.chaos.resyncReason) {
      this.emit(session, { op: 'resync_required', id: op.id,
        reason: this.chaos.resyncReason ?? (seq.cmp(after, state.head) > 0 ? 'cursor_ahead' : 'cursor_too_old'),
        oldest_available_seq: state.events[0]?.scope_seq ?? seq.inc(state.head) }); return;
    }
    session.subscriptions.set(op.id, { scope: op.scope, cursor: after });
    this.emit(session, { op: 'subscribed', id: op.id, scope: op.scope, head: state.head });
    this.pump(session, op.id);
  }
  private pump(session: HubSession, id: string): void {
    const sub = session.subscriptions.get(id); if (!sub) return;
    if ('resync' in this.range(sub.scope, sub.cursor, 1)) {
      session.subscriptions.delete(id);
      this.emit(session, { op: 'resync_required', id, reason: 'cursor_too_old', oldest_available_seq: this.journal(sub.scope).events[0]?.scope_seq ?? seq.inc(this.head(sub.scope)) });
      return;
    }
    const events = this.journal(sub.scope).events.filter(e => seq.cmp(e.scope_seq, sub.cursor) > 0);
    for (const event of mergeEvents(events, id, this.opts.mergeWindowMs, this.opts.maxFrameBytes, true, this.clock.now)) {
      if (session.closed || !session.subscriptions.has(id)) break;
      let projected = event;
      if (new TextEncoder().encode(JSON.stringify({ op: 'event', id, event })).length > (this.opts.maxFrameBytes ?? 65_536)) {
        projected = { ...event, kind: 'oversized', payload: { kind: event.kind,
          href: `/api/v1/events?scope=${event.scope}&after=${BigInt(event.scope_seq) - 1n}&limit=1` } };
      }
      sub.cursor = event.scope_seq; this.emit(session, { op: 'event', id, event: projected });
    }
  }
  flush(): void {
    this.cancelFlush?.(); this.cancelFlush = undefined;
    for (const session of this.sessions) for (const id of session.subscriptions.keys()) this.pump(session, id);
  }
  private lag(scope: string): void {
    for (const session of [...this.sessions]) {
      if (![...session.subscriptions.values()].some(s => s.scope === scope)) continue;
      for (const [id, sub] of [...session.subscriptions]) this.emit(session, { op: 'lagged', id, cursor: sub.cursor });
      this.disconnect(session, 1008);
    }
  }
}
