import { expect, vi } from 'vitest';
import { append, chat, harness } from './helpers';
import type { AppliedEvent } from '../../../src/lib/sync/scope';
import type { HubEvent } from '../../../src/lib/sync/wire';

/** Сиды 1..300 разложены по шести файлам chaos-*.test.ts, чтобы vitest гнал их разными воркерами. */
export async function runChaosSeeds(from: number, to: number) {
  for (let seed = from; seed <= to; seed++) {
    const h = await harness(seed);
    let model: HubEvent[] = [];
    h.hub.setSnapshot(chat, () => h.hub.log(chat));
    vi.mocked(h.applier.applySnapshot).mockImplementation(data => { model = (data as HubEvent[]).filter(e => e.kind !== 'hidden'); });
    vi.mocked(h.applier.applyEvents).mockImplementation((items: AppliedEvent[]) => {
      for (const { event, overlap } of items) {
        const source = h.hub.log(chat).filter(e => BigInt(e.scope_seq) <= BigInt(event.scope_seq) && e.kind !== 'hidden');
        if (overlap) model = source;
        else model.push(...source.filter(e => BigInt(e.scope_seq) >= BigInt(event.scope_seq_from)));
      }
    });
    for (let n = 1; n <= 200; n++) {
      append(h.hub, n, n % 7 === 0);
      const random = h.clock.rng();
      if (random < 0.04) h.hub.chaos.dropNext(1);
      else if (random < 0.08) h.hub.chaos.duplicateNext(1);
      else if (random < 0.12) h.hub.chaos.reorderNext();
      else if (random < 0.15) h.hub.chaos.cut();
      else if (random < 0.18) h.hub.chaos.forceResync(n % 2 ? 'cursor_ahead' : 'cursor_too_old');
      else if (random < 0.21) h.hub.chaos.restart();
      else if (random < 0.24) h.hub.chaos.lag(chat);
      if (h.clock.rng() < 0.4) h.hub.flush();
      h.wire.open(); await h.clock.advanceAsync(2);
    }
    h.hub.flush(); h.hub.chaos.cut(); h.wire.open(); await h.clock.advanceAsync(90_000);
    for (let retry = 0; retry < 200 && h.sync.cursor !== '200'; retry++) {
      h.hub.chaos.cut(); h.wire.open(); await h.clock.advanceAsync(1000);
    }
    h.sync.flushNow();
    const log = h.hub.log(chat);
    expect(log.filter(e => e.kind === 'hidden').length).toBeGreaterThanOrEqual(20);
    expect(h.sync.cursor, `seed ${seed}`).toBe('200');
    expect(model.map(e => e.scope_seq), `seed ${seed}`).toEqual(log.filter(e => e.kind !== 'hidden').map(e => e.scope_seq));
    expect(model.map(e => e.kind === 'block_delta' ? e.payload.append : '').join(''), `seed ${seed}`).toBe(log.map(e => e.kind === 'block_delta' ? e.payload.append : '').join(''));
    expect(h.probe.ledger.verify(chat, '200'), `seed ${seed}`).toEqual({ ok: true, holes: [], doubled: [] });
    h.dispose(); expect(h.clock.pending).toBe(0); expect(h.wire.listeners).toBe(0);
  }
}
