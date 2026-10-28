import type { ServerOp } from '../api/generated/ServerOp';
export type { ClientOp } from '../api/generated/ClientOp';
export type { ServerOp } from '../api/generated/ServerOp';
export type { WireEvent as HubEvent } from '../api/generated/events';

export const PROTOCOL_VERSION = 1;
export type ScopeName = 'conversations' | 'machines' | `conversation:${string}`;
export const isSeq = (value: unknown): value is string => typeof value === 'string' && /^\d+$/.test(value);
export const seq = {
  cmp(a: string, b: string): -1 | 0 | 1 { return BigInt(a) < BigInt(b) ? -1 : BigInt(a) > BigInt(b) ? 1 : 0; },
  inc(a: string): string { return (BigInt(a) + 1n).toString(); },
  max(a: string, b: string): string { return BigInt(a) > BigInt(b) ? a : b; }
};

export function parseServerFrame(text: string): ServerOp | { op: 'unknown' } {
  try {
    const f = JSON.parse(text);
    if (!f || typeof f !== 'object') return { op: 'unknown' };
    const id = typeof f.id === 'string';
    let valid = false;
    switch (f.op) {
      case 'pong': valid = Number.isFinite(f.t); break;
      case 'close': valid = f.reason === 'reauth'; break;
      case 'subscribed': valid = id && typeof f.scope === 'string' && isSeq(f.head); break;
      case 'unsubscribed': valid = id; break;
      case 'lagged': valid = id && isSeq(f.cursor); break;
      case 'resync_required': valid = id && ['cursor_too_old', 'cursor_ahead'].includes(f.reason); break;
      case 'error': valid = (f.id === undefined || id) && ['bad_scope', 'forbidden', 'too_many_subscriptions', 'bad_message'].includes(f.code); break;
      case 'event': {
        const e = f.event;
        valid = id && e && typeof e.v === 'number' && typeof e.scope === 'string' && isSeq(e.scope_seq) && isSeq(e.scope_seq_from)
          && typeof e.kind === 'string' && typeof e.id === 'string' && typeof e.committed_at === 'string'
          && typeof e.committed_at_from === 'string' && e.payload !== undefined;
        break;
      }
    }
    return valid ? f as ServerOp : { op: 'unknown' };
  } catch { return { op: 'unknown' }; }
}
