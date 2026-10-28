import type { HubEvent } from '../../src/lib/sync/wire';
import { seq } from '../../src/lib/sync/wire';

export function mergeEvents(events: HubEvent[], id: string, windowMs = 25, maxBytes = 65_536, mergeDeltas = true, now?: () => number): HubEvent[] {
  const result: HubEvent[] = [];
  let deadline = 0;
  for (const event of events) {
    const at = now?.() ?? Date.parse(event.committed_at);
    const previous = result.at(-1);
    const adjacent = previous && (!mergeDeltas || at < deadline) && previous.scope === event.scope && seq.inc(previous.scope_seq) === event.scope_seq_from;
    let merged: HubEvent | undefined;
    if (adjacent && previous.kind === 'hidden' && event.kind === 'hidden') {
      merged = { ...previous, scope_seq: event.scope_seq, committed_at: event.committed_at };
    } else if (mergeDeltas && adjacent && previous.kind === 'block_delta' && event.kind === 'block_delta'
      && previous.conversation_id === event.conversation_id && previous.execution_id === event.execution_id && previous.payload.block_id === event.payload.block_id
      && previous.payload.rev === event.payload.base_rev && event.payload.rev > event.payload.base_rev) {
      merged = { ...previous, scope_seq: event.scope_seq, committed_at: event.committed_at,
        payload: { ...previous.payload, rev: event.payload.rev, append: previous.payload.append + event.payload.append } };
    }
    if (merged && new TextEncoder().encode(JSON.stringify({ op: 'event', id, event: merged })).length <= maxBytes) result[result.length - 1] = merged;
    else { result.push(structuredClone(event)); deadline = at + Math.min(25, windowMs); }
  }
  return result;
}
