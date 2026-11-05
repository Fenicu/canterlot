import { clock } from '#lib/clock.js';

/** Столько живёт «только что»: строка, пришедшая раньше, показывается без движения. */
const FRESH_MS = 4_000;
const seen = new Set<string>();

/**
 * Строка ленты появилась на глазах: её запись свежая и показывается впервые за жизнь приложения.
 * История, открытая заново, и строка, вернувшаяся в окно ленты, входят без движения.
 */
export function arrivedNow(id: string, createdAt: number): boolean {
  if (seen.has(id)) return false;
  seen.add(id);
  return clock.serverNow() - createdAt < FRESH_MS;
}
