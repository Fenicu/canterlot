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
