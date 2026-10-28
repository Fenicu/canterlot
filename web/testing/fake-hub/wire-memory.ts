import type { Transport, TransportHandlers } from '../../src/lib/sync/transport';
import type { ClientOp } from '../../src/lib/sync/wire';
import type { FakeClock, HubPort, HubSession } from './state';

export class MemoryWire implements Transport {
  private handlers = new Set<TransportHandlers>();
  private session?: HubSession;
  private opening?: () => void;
  readonly sent: ClientOp[] = [];
  opens = 0;
  constructor(private hub: HubPort, readonly clock: FakeClock) {}
  get listeners(): number { return this.handlers.size; }
  on(h: TransportHandlers): () => void { this.handlers.add(h); return () => this.handlers.delete(h); }
  open(): void {
    if (this.session || this.opening) return;
    this.opens++;
    this.opening = this.clock.schedule(() => {
      this.opening = undefined;
      this.session = this.hub.attach({ clock: this.clock,
        message: op => { for (const h of this.handlers) h.message(JSON.stringify(op)); },
        close: code => { this.session = undefined; for (const h of this.handlers) h.close({ code, reason: '', opened: true }); }
      });
      for (const h of this.handlers) h.open();
    }, 0);
  }
  send(op: ClientOp): boolean {
    if (!this.session) return false;
    this.sent.push(structuredClone(op)); this.hub.receive(this.session, op); return true;
  }
  close(code = 1000): void {
    const opening = !!this.opening;
    this.opening?.(); this.opening = undefined;
    if (!this.session) {
      if (opening) for (const h of this.handlers) h.close({ code, reason: '', opened: false });
      return;
    }
    this.hub.detach(this.session); this.session = undefined;
    for (const h of this.handlers) h.close({ code, reason: '', opened: true });
  }
}
