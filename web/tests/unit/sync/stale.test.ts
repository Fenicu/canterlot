import { describe, expect, it } from 'vitest';
import { ConnectionStaleness, STALE_AFTER_MS } from '#lib/sync/stale.svelte.js';

/** Часы и планировщик под рукой: заморозка проверяется без настоящего времени. */
function bench(start = 1_000) {
  const clock = { t: start };
  const timers: { at: number; run: () => void; cancelled: boolean }[] = [];
  const schedule = (run: () => void, ms: number) => {
    const timer = { at: clock.t + ms, run, cancelled: false };
    timers.push(timer);
    return () => {
      timer.cancelled = true;
    };
  };
  const advance = (ms: number) => {
    clock.t += ms;
    for (const timer of timers) {
      if (!timer.cancelled && timer.at <= clock.t) {
        timer.cancelled = true;
        timer.run();
      }
    }
  };
  const stale = new ConnectionStaleness({ now: () => clock.t, schedule });
  return { clock, timers, advance, stale };
}

describe('sync/ConnectionStaleness', () => {
  it('turns_stale_only_after_five_seconds_of_a_dropped_connection', () => {
    const { advance, stale } = bench();
    stale.update(true);
    advance(STALE_AFTER_MS - 1);
    expect(stale.stale).toBe(false);
    advance(1);
    expect(stale.stale).toBe(true);
  });

  it('returns_the_last_clock_value_while_stale', () => {
    const { clock, advance, stale } = bench();
    stale.update(true);
    advance(STALE_AFTER_MS);
    const frozen = stale.now();
    expect(frozen).toBe(clock.t);
    clock.t += 60_000;
    expect(stale.now()).toBe(frozen);
    expect(stale.frozenAt).toBe(frozen);
  });

  it('restores_the_clock_at_once_when_the_connection_returns', () => {
    const { clock, advance, stale } = bench();
    stale.update(true);
    advance(STALE_AFTER_MS);
    clock.t += 3_000;
    stale.update(false);
    expect(stale.stale).toBe(false);
    expect(stale.now()).toBe(clock.t);
    expect(stale.frozenAt).toBeNull();
  });

  it('arms_one_timer_while_the_connection_stays_down', () => {
    const { timers, stale } = bench();
    stale.update(true);
    stale.update(true);
    stale.update(true);
    expect(timers.filter((timer) => !timer.cancelled)).toHaveLength(1);
  });

  it('cancels_a_pending_staleness_when_the_connection_returns', () => {
    const { advance, stale } = bench();
    stale.update(true);
    stale.update(false);
    advance(STALE_AFTER_MS * 2);
    expect(stale.stale).toBe(false);
  });
});
