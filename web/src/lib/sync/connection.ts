import { announce } from '../a11y/announce';
import { probe } from '../perf/probe';
import { backoff, type Schedule } from './backoff';
import { FrameBatcher } from './batcher';
import type { Lifecycle, ResumeReason } from './lifecycle';
import { ScopeSync, type AppliedEvent, type ScopeApplier, type ScopeStatus } from './scope';
import { connectionState } from './state.svelte';
import type { Transport } from './transport';
import { parseServerFrame, type ScopeName } from './wire';

export type ConnectionPhase = 'connecting' | 'live' | 'reconnecting' | 'offline' | 'auth_required' | 'incompatible';
export interface ConnectionStatus { phase: ConnectionPhase; since: number; lastFrameAt: number; attempt: number; resyncs: number }
export interface ConnectionDependencies {
  transport: Transport;
  lifecycle: Lifecycle;
  probeAuth(): Promise<'ok' | 'expired' | 'down'>;
  now(): number;
  schedule: Schedule;
  rng(): number;
  announce?(message: string): void;
  raf?(cb: () => void): number;
  cancelRaf?(id: number): void;
}
const labels: Record<ConnectionPhase, string> = {
  connecting: 'Подключение', live: 'Соединение установлено', reconnecting: 'Восстановление соединения',
  offline: 'Нет связи с сервером', auth_required: 'Сессия истекла. Войдите снова', incompatible: 'Обновите приложение для продолжения'
};
type Slot = { sync: ScopeSync; applier: ScopeApplier; refs: number; cancelRelease?: () => void };
const OPEN_TIMEOUT = 10_000;
export class ConnectionManager {
  readonly status: ConnectionStatus;
  private scopes = new Map<ScopeName, Slot>();
  private cleanup: (() => void)[] = [];
  private timers = new Set<() => void>();
  private disposed = false;
  private opened = false;
  private connecting = false;
  private connectStartedAt = 0;
  private selfClosing = false;
  private authRechecking = false;
  private failed = 0;
  private generation = 0;
  private immediate = false;
  private cancelReconnect?: () => void;
  private cancelPing?: () => void;
  private ping?: { t: number; at: number };
  private lastPingAt = 0;
  private nonce = 0;
  constructor(private deps: ConnectionDependencies) {
    this.status = connectionState(deps.now());
    this.cleanup.push(deps.transport.on({
      open: () => this.onOpen(), close: info => this.onClose(info), message: text => this.onMessage(text)
    }));
    this.cleanup.push(deps.lifecycle.onResume(info => this.probe(info.reason)));
    this.cleanup.push(deps.lifecycle.onSuspend(() => {
      this.cancelPing?.(); this.cancelPing = undefined; this.ping = undefined;
      for (const slot of this.scopes.values()) slot.sync.flushNow();
    }));
    this.say('connecting'); this.connect(); this.tick();
  }
  private schedule(fn: () => void, ms: number): () => void {
    let active = true;
    const raw = this.deps.schedule(() => { if (!active) return; active = false; this.timers.delete(cancel); if (!this.disposed) fn(); }, ms);
    const cancel = () => { if (!active) return; active = false; raw(); this.timers.delete(cancel); };
    this.timers.add(cancel); return cancel;
  }
  private say(phase: ConnectionPhase): void { (this.deps.announce ?? announce)(labels[phase]); }
  private phase(phase: ConnectionPhase): void {
    if (this.status.phase === phase) return;
    this.status.phase = phase; this.status.since = this.deps.now(); this.say(phase);
  }
  private connect(): void {
    if (this.disposed || this.connecting || this.opened || this.status.phase === 'incompatible' || this.status.phase === 'auth_required') return;
    this.cancelReconnect?.(); this.cancelReconnect = undefined;
    this.selfClosing = false;
    this.connecting = true; this.connectStartedAt = this.deps.now(); this.deps.transport.open();
  }
  private onOpen(): void {
    if (this.disposed) return;
    this.generation++; this.connecting = false; this.opened = true; this.selfClosing = false; this.failed = 0;
    this.status.lastFrameAt = this.lastPingAt = this.deps.now();
    this.phase('live');
    const generation = this.generation;
    this.schedule(() => { if (this.opened && generation === this.generation) this.status.attempt = 0; }, 30_000);
    for (const slot of this.scopes.values()) slot.sync.onTransportOpen();
  }
  private onClose(info: { code: number; opened: boolean }): void {
    if (this.disposed) return;
    this.generation++; this.opened = false; this.connecting = false; this.connectStartedAt = 0;
    const selfClosing = this.selfClosing; this.selfClosing = false;
    this.cancelPing?.(); this.cancelPing = undefined; this.ping = undefined;
    for (const slot of this.scopes.values()) slot.sync.onTransportClosed();
    if (this.status.phase === 'incompatible' || this.status.phase === 'auth_required') return;
    this.phase('reconnecting');
    const immediate = this.immediate || info.code === 4401;
    this.immediate = false;
    if (info.opened) this.failed = 0; else if (!selfClosing) this.failed++;
    if (this.failed >= 2) { void this.checkAuth(); return; }
    this.retry(immediate ? 0 : backoff(this.status.attempt++, this.deps.rng));
  }
  private async checkAuth(): Promise<void> {
    const generation = this.generation;
    let result: 'ok' | 'expired' | 'down';
    try { result = await this.deps.probeAuth(); } catch { result = 'down'; }
    if (this.disposed || generation !== this.generation) return;
    if (result === 'expired') { this.phase('auth_required'); return; }
    this.phase(result === 'down' ? 'offline' : 'reconnecting');
    this.retry(backoff(this.status.attempt++, this.deps.rng));
  }
  /** Повторная проверка входа после истечения сессии: без явного отказа снова подключаемся. */
  private async recheckAuth(): Promise<void> {
    if (this.authRechecking || this.disposed || this.status.phase !== 'auth_required') return;
    this.authRechecking = true;
    const generation = this.generation;
    let result: 'ok' | 'expired' | 'down';
    try { result = await this.deps.probeAuth(); } catch { result = 'down'; }
    this.authRechecking = false;
    if (this.disposed || generation !== this.generation || this.status.phase !== 'auth_required') return;
    if (result !== 'ok') return;
    this.failed = 0;
    this.phase('reconnecting');
    this.connect();
  }
  private retry(ms: number): void {
    this.cancelReconnect?.();
    this.cancelReconnect = this.schedule(() => { this.cancelReconnect = undefined; this.connect(); }, ms);
  }
  private restart(immediate = true): void {
    if (this.disposed || this.status.phase === 'incompatible' || this.status.phase === 'auth_required') return;
    this.cancelReconnect?.(); this.cancelReconnect = undefined;
    if (this.opened || this.connecting) { this.immediate = immediate; this.selfClosing = true; this.deps.transport.close(); }
    else this.retry(0);
  }
  private onMessage(text: string): void {
    if (this.disposed || !this.opened || this.status.phase === 'incompatible') return;
    this.status.lastFrameAt = this.deps.now();
    const op = parseServerFrame(text);
    if (op.op === 'unknown') return;
    if (op.op === 'close') { this.restart(); return; }
    if (op.op === 'pong' && op.t === this.ping?.t) { this.cancelPing?.(); this.cancelPing = undefined; this.ping = undefined; }
    for (const slot of this.scopes.values()) slot.sync.onFrame(op);
  }
  private incompatible(): void {
    this.phase('incompatible'); this.cancelReconnect?.(); this.cancelPing?.();
    for (const slot of this.scopes.values()) slot.sync.incompatible();
    this.deps.transport.close();
  }
  private sendPing(deadline: number): void {
    if (!this.opened || this.ping) return;
    const t = ++this.nonce;
    this.lastPingAt = this.deps.now(); this.ping = { t, at: this.lastPingAt };
    this.cancelPing = this.schedule(() => { this.cancelPing = undefined; this.ping = undefined; this.restart(deadline === 2000); }, deadline);
    if (!this.deps.transport.send({ op: 'ping', t })) this.restart(deadline === 2000);
  }
  private tick(): void {
    if (this.disposed) return;
    const now = this.deps.now();
    if (this.connecting && now - this.connectStartedAt >= OPEN_TIMEOUT) this.restart(false);
    else if (this.deps.lifecycle.visible && this.opened && this.status.phase === 'live') {
      if (!this.ping && now - this.status.lastFrameAt >= 45_000) this.restart(false);
      else if (now - this.lastPingAt >= 25_000) this.sendPing(5000);
    }
    this.schedule(() => this.tick(), 1000);
  }
  subscribe(scope: ScopeName, applier: ScopeApplier): { release(): void; readonly status: ScopeStatus; readonly cursor: string } {
    if (this.disposed) throw new Error('connection_disposed');
    let slot = this.scopes.get(scope);
    // Отпущенный слот с чужим получателем доставлял бы события уже закрытому хранилищу.
    if (slot && !slot.refs && slot.applier !== applier) { this.drop(scope, slot); slot = undefined; }
    if (!slot) {
      if (this.scopes.size >= 16) {
        const idle = [...this.scopes].find(([, item]) => !item.refs);
        if (!idle) throw new Error('too_many_subscriptions');
        this.drop(idle[0], idle[1]);
      }
      const batcher = new FrameBatcher<AppliedEvent>(items => applier.applyEvents(items), {
        raf: this.deps.raf, cancelRaf: this.deps.cancelRaf, now: () => performance.now(),
        visible: () => this.deps.lifecycle.visible, schedule: (fn, ms) => this.schedule(fn, ms)
      });
      const sync = new ScopeSync(scope, applier, {
        send: op => this.opened && this.deps.transport.send(op), now: this.deps.now, rng: this.deps.rng,
        schedule: (fn, ms) => this.schedule(fn, ms), batcher, probe,
        onIncompatible: () => this.incompatible(), onResync: () => { this.status.resyncs++; }
      });
      slot = { sync, applier, refs: 0 }; this.scopes.set(scope, slot);
      sync.start(); if (this.opened) sync.onTransportOpen();
      if (this.status.phase === 'incompatible') sync.incompatible();
    }
    slot.refs++; slot.cancelRelease?.(); slot.cancelRelease = undefined;
    const current = slot; let released = false;
    return {
      get status() { return current.sync.status; }, get cursor() { return current.sync.cursor; },
      release: () => {
        if (released || this.disposed) return;
        released = true; current.refs--;
        if (!current.refs) current.cancelRelease = this.schedule(() => this.drop(scope, current), 30_000);
      }
    };
  }
  private drop(scope: ScopeName, slot: Slot): void {
    slot.cancelRelease?.(); slot.cancelRelease = undefined;
    slot.sync.stop();
    if (this.scopes.get(scope) === slot) this.scopes.delete(scope);
  }
  probe(reason: ResumeReason | 'manual'): void {
    if (this.disposed || this.status.phase === 'incompatible') return;
    if (this.status.phase === 'auth_required') {
      if (reason === 'manual') {
        this.failed = 0; this.generation++; this.phase('connecting'); this.connect();
      } else void this.recheckAuth();
      return;
    }
    if (this.opened) {
      this.cancelPing?.(); this.cancelPing = undefined; this.ping = undefined;
      this.sendPing(2000);
    } else if (this.connecting) this.restart();
    else { this.generation++; this.connect(); }
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const slot of this.scopes.values()) slot.sync.stop();
    this.scopes.clear();
    for (const cancel of this.timers) cancel(); this.timers.clear();
    for (const release of this.cleanup) release(); this.cleanup = [];
    this.deps.transport.close();
  }
}
