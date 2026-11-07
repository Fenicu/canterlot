import { describe, expect, it } from 'vitest';
import { forecastView, paceText, windowText, type WindowForecast } from '#lib/stores/limits.js';
import { currentBlocks } from '#lib/screens/limits/model.js';

const NOW = Date.UTC(2026, 10, 8, 12, 0);
const H = 3_600_000;
const N = ' ';
const iso = (ms: number) => new Date(NOW + ms).toISOString();
const forecast = (patch: Partial<WindowForecast> = {}): WindowForecast => ({ rate_permille_per_hour: 42, recent_rate_permille_per_hour: null, exhausts_at: iso(3 * H), before_reset: true, ...patch });
const cfg = { stale_after_secs: 35 * 60, warn_pcts: [80, 95], weekly_reserve_pct: 95, warn_remaining_pct: 20, danger_remaining_pct: 5, reserve_remaining_pct: 5 };

describe('limits forecast text', () => {
  it('warns only when the window runs out before the reset', () => {
    expect(forecastView(forecast(), 700, NOW)).toEqual({ before: true, text: `кончится через 3${N}ч`, long: 'кончится через 3 часа', pace: `≈4${N}%/ч` });
    expect(forecastView(forecast({ exhausts_at: iso(5 * H + 10 * 60_000) }), 700, NOW)?.long).toBe('кончится через 5 часов');
    expect(forecastView(forecast({ exhausts_at: iso(H * 21) }), 700, NOW)?.long).toBe('кончится через 21 час');
  });

  it('calm text when the pace lasts until the reset or is zero', () => {
    expect(forecastView(forecast({ before_reset: false, exhausts_at: iso(90 * H) }), 300, NOW)).toMatchObject({ before: false, text: 'хватит до сброса' });
    expect(forecastView(forecast({ before_reset: false, exhausts_at: null, rate_permille_per_hour: 0 }), 300, NOW)).toMatchObject({ before: false, text: 'хватит до сброса', pace: `≈0${N}%/ч` });
  });

  it('no field, an exhausted window, and a past exhaustion moment', () => {
    expect(forecastView(undefined, 300, NOW)).toBeNull();
    expect(forecastView(null, 300, NOW)).toBeNull();
    expect(forecastView(forecast(), 1000, NOW)).toBeNull();
    expect(forecastView(forecast({ exhausts_at: iso(-60_000) }), 990, NOW)?.text).toBe('кончится с минуты на минуту');
  });

  it('pace is a whole percent per hour, tenths below one percent', () => {
    expect(paceText(42)).toBe(`≈4${N}%/ч`);
    expect(paceText(125)).toBe(`≈13${N}%/ч`);
    expect(paceText(5)).toBe(`≈0,5${N}%/ч`);
    expect(paceText(0)).toBe(`≈0${N}%/ч`);
  });

  it('a stale window drops the forecast and the countdown', () => {
    const window = { used_permille: 500, resets_at: iso(-60_000), observed_at: iso(-2 * H), forecast: forecast() };
    const meter = windowText(window, NOW, { staleAfterMs: 35 * 60_000 });
    expect(meter.stale).toBe(true);
    expect(meter.forecast).toBeNull();
    expect(meter.reset).toBe('');
  });

  it('limits screen rows carry the forecast and the reset moment for the date tooltip', () => {
    const blocks = currentBlocks([{
      provider: 'claude', weekly_reserve_active: false,
      five_hour: { used_permille: 300, resets_at: iso(2 * H), observed_at: iso(-60_000), forecast: forecast({ before_reset: false, exhausts_at: iso(9 * H) }) },
      weekly: { used_permille: 700, resets_at: iso(30 * H), observed_at: iso(-60_000), forecast: forecast() }
    }], NOW, cfg);
    const [five, week] = blocks[0]!.windows;
    expect(five!.forecast).toMatchObject({ before: false, text: 'хватит до сброса' });
    expect(week!.forecast).toMatchObject({ before: true, text: `кончится через 3${N}ч` });
    expect(week!.reset).toBe(`сброс через 30${N}ч`);
    expect(week!.resetLong).toBe('сброс через 30 часов');
    expect(week!.resetAt).toBe(NOW + 30 * H);
  });
});
