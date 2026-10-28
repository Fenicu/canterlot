import type { ClientOp, HubEvent, ServerOp } from '../../src/lib/sync/wire';
import type { Schedule } from '../../src/lib/sync/backoff';

export interface HubClock { now(): number; schedule: Schedule }
export class FakeClock implements HubClock {
  private time = 0;
  private serial = 0;
  private seed: number;
  private timers = new Map<number, { at: number; fn: () => void }>();
  constructor(seed = 1) { this.seed = seed >>> 0; }
  now = (): number => this.time;
  rng = (): number => { this.seed = (Math.imul(this.seed, 1664525) + 1013904223) >>> 0; return this.seed / 4294967296; };
  schedule: Schedule = (fn, ms) => {
    const id = ++this.serial; this.timers.set(id, { at: this.time + Math.max(0, ms), fn });
    return () => { this.timers.delete(id); };
  };
  raf = (fn: () => void): number => {
    const id = ++this.serial;
    this.timers.set(id, { at: (Math.floor(this.time / 16) + 1) * 16, fn }); return id;
  };
  cancelRaf = (id: number): void => { this.timers.delete(id); };
  get pending(): number { return this.timers.size; }
  advance(ms: number): void {
    const end = this.time + ms;
    let guard = 0;
    for (;;) {
      const next = [...this.timers].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
      if (!next) break;
      if (++guard > 1_000_000) throw new Error('timer_loop');
      this.time = next[1].at; this.timers.delete(next[0]); next[1].fn();
    }
    this.time = end;
  }
  async advanceAsync(ms: number): Promise<void> {
    const end = this.time + ms;
    for (let i = 0; i < 12; i++) await Promise.resolve();
    for (;;) {
      const next = [...this.timers.values()].filter(t => t.at <= end).sort((a, b) => a.at - b.at)[0];
      if (!next) break;
      this.advance(next.at - this.time);
      for (let i = 0; i < 12; i++) await Promise.resolve();
    }
    this.advance(end - this.time);
    for (let i = 0; i < 12; i++) await Promise.resolve();
  }
}
export interface HubPeer { message(op: ServerOp): void; close(code: number): void; clock: HubClock }
export interface HubSession {
  peer: HubPeer;
  subscriptions: Map<string, { scope: string; cursor: string }>;
  closed: boolean;
  blocked: boolean;
  pending: Set<() => void>;
  held?: ServerOp;
}
export interface HubPort {
  attach(peer: HubPeer): HubSession;
  receive(session: HubSession, op: ClientOp): void;
  detach(session: HubSession): void;
}
export interface ScopeJournal { head: string; events: HubEvent[]; build: () => unknown }
