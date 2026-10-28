import type { ClientOp } from './wire';

export interface TransportHandlers {
  open(): void;
  close(i: { code: number; reason: string; opened: boolean }): void;
  message(text: string): void;
}
export interface Transport {
  open(): void;
  send(op: ClientOp): boolean;
  close(code?: number): void;
  on(h: TransportHandlers): () => void;
}
export class BrowserSocket implements Transport {
  private socket?: WebSocket;
  private didOpen = false;
  private handlers = new Set<TransportHandlers>();
  constructor(private url: () => string) {}
  on(h: TransportHandlers): () => void { this.handlers.add(h); return () => this.handlers.delete(h); }
  open(): void {
    if (this.socket) return;
    this.didOpen = false;
    try {
      const socket = new WebSocket(this.url(), 'canterlot.v1');
      this.socket = socket;
      socket.onopen = () => { if (this.socket !== socket) return; this.didOpen = true; for (const h of this.handlers) h.open(); };
      socket.onmessage = e => { if (this.socket === socket && typeof e.data === 'string') for (const h of this.handlers) h.message(e.data); };
      socket.onerror = () => { if (this.socket === socket) socket.close(); };
      socket.onclose = e => {
        if (this.socket !== socket) return;
        const opened = this.didOpen; this.didOpen = false;
        this.socket = undefined;
        for (const h of this.handlers) h.close({ code: e.code, reason: e.reason, opened });
      };
    } catch {
      this.socket = undefined;
      for (const h of this.handlers) h.close({ code: 1006, reason: '', opened: false });
    }
  }
  send(op: ClientOp): boolean {
    if (this.socket?.readyState !== WebSocket.OPEN) return false;
    try { this.socket.send(JSON.stringify(op)); return true; } catch { return false; }
  }
  close(code = 1000): void {
    const socket = this.socket;
    if (!socket) return;
    this.socket = undefined;
    socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
    const opened = this.didOpen; this.didOpen = false;
    socket.close(code);
    for (const h of this.handlers) h.close({ code, reason: '', opened });
  }
}
