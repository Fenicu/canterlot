export type DurationToken = `--duration-${string}`;
export function prefersReducedMotion(): boolean {
  return typeof window === 'undefined' || (typeof window.matchMedia === 'function' && window.matchMedia('(prefers-reduced-motion: reduce)').matches) || getComputedStyle(document.documentElement).getPropertyValue('--motion-scale').trim() === '0';
}
export function motionScale(): number {
  if (typeof document === 'undefined') return 1;
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue('--motion-scale')
    .trim();
  if (raw === '') return 1;
  const value = Number(raw);
  return Number.isFinite(value) ? value : 1;
}
export function durationMs(token: DurationToken): number {
  if (prefersReducedMotion()) return 0;
  const probe = document.createElement('span');
  probe.style.transitionDuration = `var(${token})`;
  probe.hidden = true;
  document.documentElement.append(probe);
  const value = getComputedStyle(probe).transitionDuration;
  probe.remove();
  const numeric = parseFloat(value);
  return Number.isFinite(numeric) ? numeric * (value.endsWith('ms') ? 1 : 1000) : 0;
}
export function scaledDurationMs(milliseconds: number): number {
  if (prefersReducedMotion()) return 0;
  const scale = Number(getComputedStyle(document.documentElement).getPropertyValue('--motion-scale').trim());
  return Math.max(0, milliseconds) * (Number.isFinite(scale) ? scale : 1);
}
export function setMotionScale(scale: 0 | 0.5 | 1 | 1.5): void {
  document.documentElement.style.setProperty('--motion-scale', String(scale));
  document.documentElement.dataset.motion = scale === 0 ? 'off' : 'on';
}
export type EaseToken = `--ease-${string}`;
/** Кривая токена для переходов Svelte: CSS её не читает, поэтому `cubic-bezier` из токена решается здесь. */
export function easing(token: EaseToken): (t: number) => number {
  const raw = typeof document === 'undefined' ? '' : getComputedStyle(document.documentElement).getPropertyValue(token);
  const points = raw.match(/cubic-bezier\(([^)]*)\)/)?.[1]?.split(',').map(Number);
  if (!points || points.length !== 4 || points.some((value) => !Number.isFinite(value))) return (t) => t;
  return cubicBezier(points[0]!, points[1]!, points[2]!, points[3]!);
}
export function cubicBezier(x1: number, y1: number, x2: number, y2: number): (t: number) => number {
  const at = (a: number, b: number, s: number) => 3 * a * s * (1 - s) ** 2 + 3 * b * s * s * (1 - s) + s ** 3;
  const slope = (a: number, b: number, s: number) => 3 * a * (1 - s) ** 2 + 6 * (b - a) * s * (1 - s) + 3 * (1 - b) * s * s;
  return (t) => {
    if (t <= 0) return 0;
    if (t >= 1) return 1;
    let s = t;
    for (let step = 0; step < 8; step++) {
      const error = at(x1, x2, s) - t;
      if (Math.abs(error) < 1e-5) break;
      const d = slope(x1, x2, s);
      if (Math.abs(d) < 1e-6) break;
      s = Math.min(1, Math.max(0, s - error / d));
    }
    // Ньютон на пологом участке может застрять: доводим делением пополам.
    if (Math.abs(at(x1, x2, s) - t) > 1e-4) {
      let low = 0;
      let high = 1;
      s = t;
      for (let step = 0; step < 30; step++) {
        if (at(x1, x2, s) < t) low = s;
        else high = s;
        s = (low + high) / 2;
      }
    }
    return at(y1, y2, s);
  };
}
