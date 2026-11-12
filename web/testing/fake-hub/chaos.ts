import type { ServerOp } from '../../src/lib/sync/wire';
import type { HubSession } from './state';

export class Chaos {
  private drops = 0;
  private duplicates = 0;
  private reorder = false;
  delayMs = 0;
  resyncReason?: 'cursor_too_old' | 'cursor_ahead';
  constructor(private sessions: Set<HubSession>, private disconnect: (s: HubSession, code: number) => void, private lagScope: (scope: string) => void) {}
  dropNext(n: number): void { this.drops += n; }
  duplicateNext(n: number): void { this.duplicates += n; }
  reorderNext(): void { this.reorder = true; }
  delay(ms: number): void { this.delayMs = ms; }
  blackhole(on: boolean): void { for (const session of this.sessions) session.blocked = on; }
  cut(): void { for (const session of [...this.sessions]) this.disconnect(session, 1006); }
  close4401(): void {
    for (const session of [...this.sessions]) { session.peer.message({ op: 'close', reason: 'reauth' }); this.disconnect(session, 4401); }
  }
  /** Хаб отозвал сеанс: кадр close с причиной revoked и код 4401. */
  closeRevoked(): void {
    for (const session of [...this.sessions]) { session.peer.message({ op: 'close', reason: 'revoked' }); this.disconnect(session, 4401); }
  }
  forceResync(reason: 'cursor_too_old' | 'cursor_ahead'): void {
    this.resyncReason = reason;
    for (const session of this.sessions) for (const [id] of [...session.subscriptions]) {
      session.subscriptions.delete(id);
      session.peer.message({ op: 'resync_required', id, reason, oldest_available_seq: '1' });
    }
    this.resyncReason = undefined;
  }
  lag(scope: string): void { this.lagScope(scope); }
  restart(): void { this.cut(); }
  deliver(session: HubSession, op: ServerOp): void {
    if (session.closed || session.blocked) return;
    const send = (frame: ServerOp) => {
      if (!this.delayMs) { session.peer.message(frame); return; }
      const cancel = session.peer.clock.schedule(() => {
        session.pending.delete(cancel);
        if (!session.closed && !session.blocked && (frame.op !== 'event' || session.subscriptions.has(frame.id))) session.peer.message(frame);
      }, this.delayMs);
      session.pending.add(cancel);
    };
    if (op.op !== 'event') { send(op); return; }
    if (this.drops > 0) { this.drops--; return; }
    if (this.reorder) { this.reorder = false; session.held = op; return; }
    send(op);
    if (this.duplicates > 0) { this.duplicates--; send(op); }
    if (session.held) { const held = session.held; session.held = undefined; send(held); }
  }
}
