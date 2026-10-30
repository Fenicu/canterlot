import { FrameBatcher } from './batcher';
import { backoff, type Schedule } from './backoff';
import { PROTOCOL_VERSION, isSeq, seq, type ClientOp, type HubEvent, type ScopeName, type ServerOp } from './wire';
import { probe, type Probe } from '../perf/probe';
import { scopeState } from './state.svelte';

export type ScopeStatus = 'idle' | 'loading_snapshot' | 'subscribing' | 'live' | 'gap_recovery' | 'reconnecting' | 'forbidden' | 'deleted' | 'incompatible' | 'degraded';
export type RangeResult = { events: HubEvent[]; head: string } | { resync: true };
export interface AppliedEvent { event: HubEvent; overlap: boolean }
export interface ScopeApplier {
  loadSnapshot(): Promise<{ cursor: string; data: unknown }>;
  applySnapshot(data: unknown, cursor: string): void;
  applyEvents(batch: AppliedEvent[]): void;
  fetchRange(after: string, limit: number): Promise<RangeResult>;
}
export interface ScopeEnv {
  send(op: ClientOp): boolean;
  now(): number;
  schedule: Schedule;
  rng(): number;
  batcher?: FrameBatcher<AppliedEvent>;
  probe?: Probe;
  onIncompatible?(): void;
  onResync?(): void;
}
let subscriptionId = 0;
export class ScopeSync {
  private state = scopeState();
  private started = false;
  private connected = false;
  private hasSnapshot = false;
  private loading = false;
  private id?: string;
  private epoch = 0;
  private recovering = false;
  private buffer: HubEvent[] = [];
  private buffered = new Set<string>();
  private bytes = 0;
  private cancelRetry?: () => void;
  private waitingSnapshot = false;
  private resyncTimes: number[] = [];
  private lagTimes: number[] = [];
  private batcher: FrameBatcher<AppliedEvent>;
  private probe: Probe;
  constructor(readonly scope: ScopeName, private applier: ScopeApplier, private env: ScopeEnv) {
    this.batcher = env.batcher ?? new FrameBatcher(items => applier.applyEvents(items));
    this.probe = env.probe ?? probe;
  }
  get cursor(): string { return this.state.cursor; }
  get status(): ScopeStatus { return this.state.status; }
  private get terminal(): boolean { return this.status === 'forbidden' || this.status === 'deleted' || this.status === 'incompatible'; }
  start(): void { if (!this.started) { this.started = true; void this.snapshot(); } }
  stop(): void {
    this.unsubscribe(); this.started = false; this.epoch++; this.cancelRetry?.(); this.cancelRetry = undefined;
    this.buffer = []; this.buffered.clear(); this.bytes = 0; this.batcher.dispose(); this.state.status = 'idle';
  }
  flushNow(): void { this.batcher.flushNow(); }
  incompatible(): void {
    this.unsubscribe(); this.epoch++; this.cancelRetry?.(); this.cancelRetry = undefined;
    this.batcher.flushNow(); this.state.status = 'incompatible';
  }
  onTransportOpen(): void {
    this.connected = true;
    if (this.started && this.hasSnapshot && !this.loading && !this.cancelRetry && !this.terminal) this.subscribe();
  }
  onTransportClosed(): void {
    this.connected = false; this.id = undefined;
    if (this.terminal || !this.started) return;
    if (this.cancelRetry && !this.waitingSnapshot) { this.cancelRetry(); this.cancelRetry = undefined; }
    if (!this.loading) { this.epoch++; this.recovering = false; this.buffer = []; this.buffered.clear(); this.bytes = 0; }
    if (!this.loading && !this.cancelRetry) this.state.status = 'reconnecting';
  }
  private subscribe(): void {
    if (!this.connected || this.terminal || this.id) return;
    this.id = `s${++subscriptionId}`;
    this.state.status = 'subscribing';
    if (!this.env.send({ op: 'subscribe', id: this.id, scope: this.scope, after: this.cursor })) {
      this.id = undefined; this.state.status = 'reconnecting';
    }
  }
  private unsubscribe(): void {
    const id = this.id; this.id = undefined;
    if (id && this.connected) this.env.send({ op: 'unsubscribe', id });
  }
  private async snapshot(): Promise<void> {
    const epoch = ++this.epoch;
    this.loading = true; this.recovering = false;
    this.buffer = []; this.buffered.clear(); this.bytes = 0;
    this.state.status = 'loading_snapshot';
    this.batcher.flushNow();
    try {
      const result = await this.applier.loadSnapshot();
      if (!this.started || epoch !== this.epoch || this.terminal) return;
      if (!isSeq(result.cursor)) throw new Error('invalid_snapshot_cursor');
      this.applier.applySnapshot(result.data, result.cursor);
      if (this.probe.enabled) {
        const baseline = !this.hasSnapshot || seq.cmp(result.cursor, this.cursor) < 0;
        if (baseline) this.probe.ledger.reset(this.scope);
        if (baseline || seq.cmp(result.cursor, this.cursor) > 0)
          this.probe.ledger.record(this.scope, baseline ? '1' : seq.inc(this.cursor), result.cursor, 'snapshot');
      }
      this.state.cursor = result.cursor; this.hasSnapshot = true;
      this.loading = false;
      if (this.connected) this.subscribe(); else this.state.status = 'reconnecting';
    } catch (error) {
      if (!this.started || epoch !== this.epoch || this.terminal) return;
      this.loading = false;
      const code = (error as { code?: unknown } | null)?.code;
      if (code === 'conversation_deleted') { this.state.status = 'deleted'; return; }
      if (code === 'not_found') { this.state.status = 'forbidden'; return; }
      this.resync();
    }
  }
  private resync(): void {
    if (!this.started || this.terminal || this.loading || (this.cancelRetry && this.waitingSnapshot)) return;
    this.cancelRetry?.(); this.cancelRetry = undefined;
    this.unsubscribe(); this.epoch++; this.recovering = false; this.buffer = []; this.buffered.clear(); this.bytes = 0;
    const now = this.env.now();
    this.resyncTimes = this.resyncTimes.filter(at => now - at < 60_000);
    this.resyncTimes.push(now); this.env.onResync?.();
    const count = this.resyncTimes.length;
    const delay = count === 1 ? 0 : count === 2 ? backoff(0, this.env.rng) : 30_000;
    if (delay === 0) { void this.snapshot(); return; }
    this.state.status = count >= 3 ? 'degraded' : 'reconnecting';
    this.waitingSnapshot = true;
    this.cancelRetry = this.env.schedule(() => { this.cancelRetry = undefined; this.waitingSnapshot = false; void this.snapshot(); }, delay);
  }
  onFrame(op: ServerOp): void {
    if (!this.started || this.terminal || !('id' in op) || !this.id || op.id !== this.id) return;
    switch (op.op) {
      case 'subscribed': if (op.scope === this.scope) this.state.status = 'live'; break;
      case 'event':
        if (op.event.scope !== this.scope) return;
        if (op.event.v !== PROTOCOL_VERSION) { this.incompatible(); this.env.onIncompatible?.(); return; }
        this.receive(op.event); break;
      case 'resync_required': this.resync(); break;
      case 'lagged': {
        const now = this.env.now(); this.lagTimes = this.lagTimes.filter(at => now - at < 60_000); this.lagTimes.push(now);
        if (this.lagTimes.length > 1) this.resync(); else this.state.status = 'reconnecting';
        break;
      }
      case 'error':
        this.unsubscribe(); this.epoch++; this.recovering = false;
        this.cancelRetry?.(); this.cancelRetry = undefined; this.buffer = []; this.buffered.clear(); this.bytes = 0;
        this.state.status = op.code === 'forbidden' || op.code === 'bad_scope' ? 'forbidden' : 'degraded';
        break;
    }
  }
  private valid(event: HubEvent): boolean {
    if (!isSeq(event.scope_seq) || !isSeq(event.scope_seq_from) || seq.cmp(event.scope_seq_from, event.scope_seq) > 0) {
      this.probe.count('invalid_range'); return false;
    }
    return event.scope === this.scope;
  }
  private receive(event: HubEvent): void {
    if (!this.valid(event) || seq.cmp(event.scope_seq, this.cursor) <= 0) return;
    if (seq.cmp(event.scope_seq_from, seq.inc(this.cursor)) <= 0 && event.kind !== 'oversized') {
      this.accept(event); this.drain();
    } else {
      const key = `${event.scope_seq}:${event.kind}`;
      if (!this.buffered.has(key)) {
        this.buffered.add(key); this.bytes += new TextEncoder().encode(JSON.stringify(event)).length;
        let lo = 0, hi = this.buffer.length;
        while (lo < hi) {
          const mid = (lo + hi) >>> 1, other = this.buffer[mid]!;
          if ((seq.cmp(other.scope_seq_from, event.scope_seq_from) || seq.cmp(other.scope_seq, event.scope_seq)) <= 0) lo = mid + 1;
          else hi = mid;
        }
        this.buffer.splice(lo, 0, event);
      }
      if (this.buffer.length > 5000 || this.bytes > 8 * 1024 * 1024) { this.resync(); return; }
      void this.recover();
    }
  }
  private accept(event: HubEvent, how?: 'range' | 'refetch'): void {
    const previous = this.cursor;
    const overlap = seq.cmp(event.scope_seq_from, previous) <= 0;
    this.state.cursor = event.scope_seq;
    if (this.probe.enabled) {
      this.probe.ledger.record(this.scope, seq.inc(previous), this.cursor, how ?? (event.scope_seq_from === event.scope_seq ? 'single' : 'merged'));
      this.probe.record('event_age_ms', Math.max(0, this.env.now() - Date.parse(event.committed_at_from)));
    }
    if (event.kind !== 'hidden') this.batcher.push({ event, overlap });
  }
  private drain(): void {
    while (this.buffer.length) {
      const event = this.buffer[0]!;
      if (seq.cmp(event.scope_seq, this.cursor) > 0 && (seq.cmp(event.scope_seq_from, seq.inc(this.cursor)) > 0 || event.kind === 'oversized')) break;
      this.buffer.shift(); this.bytes -= new TextEncoder().encode(JSON.stringify(event)).length;
      this.buffered.delete(`${event.scope_seq}:${event.kind}`);
      if (seq.cmp(event.scope_seq, this.cursor) > 0) this.accept(event);
    }
    if (!this.buffer.length && this.status === 'gap_recovery') this.state.status = 'live';
  }
  private async recover(): Promise<void> {
    if (this.recovering || this.loading || this.cancelRetry || this.terminal) return;
    this.recovering = true; this.state.status = 'gap_recovery';
    const epoch = this.epoch;
    try {
      while (this.buffer.length && epoch === this.epoch) {
        this.drain(); if (!this.buffer.length) break;
        const first = this.buffer[0]!;
        const refetch = first.kind === 'oversized' && seq.cmp(first.scope_seq_from, seq.inc(this.cursor)) <= 0;
        const after = refetch ? (BigInt(first.scope_seq) - 1n).toString() : this.cursor;
        const result = await this.applier.fetchRange(after, refetch ? 1 : 256);
        if (!this.started || epoch !== this.epoch) return;
        if ('resync' in result) { this.resync(); return; }
        const before = this.cursor;
        for (const event of [...result.events].sort((a, b) => seq.cmp(a.scope_seq_from, b.scope_seq_from))) {
          if (event.v !== PROTOCOL_VERSION) { this.incompatible(); this.env.onIncompatible?.(); return; }
          if (!this.valid(event) || seq.cmp(event.scope_seq, this.cursor) <= 0) continue;
          if (event.kind === 'oversized' || seq.cmp(event.scope_seq_from, seq.inc(this.cursor)) > 0) break;
          this.accept(event, refetch ? 'refetch' : 'range');
        }
        this.drain();
        if (this.cursor === before && this.buffer.length) { this.resync(); return; }
      }
    } catch {
      if (this.started && epoch === this.epoch) {
        this.state.status = 'gap_recovery';
        this.cancelRetry = this.env.schedule(() => { this.cancelRetry = undefined; void this.recover(); }, backoff(0, this.env.rng));
      }
    } finally { if (epoch === this.epoch) this.recovering = false; }
  }
}
