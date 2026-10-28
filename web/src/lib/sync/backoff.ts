export function backoff(attempt: number, rng: () => number): number {
  return Math.min(15_000, Math.round(250 * 2 ** Math.min(attempt, 10) * (0.7 + 0.6 * rng())));
}
export type Schedule = (fn: () => void, ms: number) => () => void;
export const schedule: Schedule = (fn, ms) => { const timer = setTimeout(fn, ms); return () => clearTimeout(timer); };
