export type DurationToken = `--duration-${string}`;
const REDUCED_QUERY = '(prefers-reduced-motion: reduce)';
/**
 * Значения движения читаются из вычисленного стиля, и каждое чтение пересчитывает стиль документа:
 * лента звала их на каждую смонтированную строку. Снимок держится, пока не сменится то, от чего
 * зависят токены длительности: системная настройка движения, свой стиль корня (`--motion-scale`
 * пишется туда) и набор таблиц стилей. Тема и палитра длительностей не задают: они объявлены
 * только в `:root` (проверяет `duration_tokens_live_only_in_root`).
 */
const snapshot = {
  key: '',
  doc: null as Document | null,
  matcher: null as typeof window.matchMedia | null,
  query: null as MediaQueryList | null,
  root: new Map<string, string>(),
  probed: new Map<string, number>()
};
function reducedQuery(): boolean {
  if (typeof window.matchMedia !== 'function') return false;
  // `matches` у держимого списка свежий сразу, а событие `change` приходит только со следующим кадром.
  if (snapshot.matcher !== window.matchMedia || !snapshot.query) {
    snapshot.matcher = window.matchMedia;
    snapshot.query = window.matchMedia(REDUCED_QUERY);
    snapshot.key = '';
  }
  return snapshot.query.matches;
}
function current(): boolean {
  const reduced = reducedQuery();
  const root = document.documentElement;
  const key = `${reduced ? 1 : 0}|${document.styleSheets.length}|${root.getAttribute('style') ?? ''}`;
  if (snapshot.doc !== document || snapshot.key !== key) {
    snapshot.doc = document;
    snapshot.key = key;
    snapshot.root.clear();
    snapshot.probed.clear();
  }
  return reduced;
}
function rootValue(name: string): string {
  current();
  let value = snapshot.root.get(name);
  if (value === undefined) {
    value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    snapshot.root.set(name, value);
  }
  return value;
}
function probeMs(token: DurationToken): number {
  current();
  let ms = snapshot.probed.get(token);
  if (ms === undefined) {
    const probe = document.createElement('span');
    probe.style.transitionDuration = `var(${token})`;
    probe.hidden = true;
    document.documentElement.append(probe);
    const value = getComputedStyle(probe).transitionDuration;
    probe.remove();
    ms = toMs(value);
    snapshot.probed.set(token, ms);
  }
  return ms;
}
function toMs(raw: string): number {
  const numeric = parseFloat(raw);
  return Number.isFinite(numeric) ? numeric * (raw.endsWith('ms') ? 1 : 1000) : 0;
}
export function prefersReducedMotion(): boolean {
  if (typeof window === 'undefined') return true;
  return current() || rootValue('--motion-scale') === '0';
}
export function motionScale(): number {
  if (typeof document === 'undefined') return 1;
  const raw = rootValue('--motion-scale');
  if (raw === '') return 1;
  const value = Number(raw);
  return Number.isFinite(value) ? value : 1;
}
export function durationMs(token: DurationToken): number {
  if (prefersReducedMotion()) return 0;
  return probeMs(token);
}
export function scaledDurationMs(milliseconds: number): number {
  if (prefersReducedMotion()) return 0;
  const scale = Number(rootValue('--motion-scale'));
  return Math.max(0, milliseconds) * (Number.isFinite(scale) ? scale : 1);
}
export function setMotionScale(scale: 0 | 0.5 | 1 | 1.5): void {
  document.documentElement.style.setProperty('--motion-scale', String(scale));
  document.documentElement.dataset.motion = scale === 0 ? 'off' : 'on';
}
export type EaseToken = `--ease-${string}`;
/** Кривая токена для переходов Svelte: CSS её не читает, поэтому `cubic-bezier` из токена решается здесь. */
export function easing(token: EaseToken): (t: number) => number {
  const raw = typeof document === 'undefined' ? '' : rootValue(token);
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
/**
 * Длительность смены прозрачности. Она ничего не сдвигает, поэтому при выключенном движении не пропадает,
 * а идёт с базовой длительностью токена: просмотрщик картинки так появляется вместо въезда из миниатюры.
 */
export function fadeMs(token: DurationToken): number {
  if (typeof document === 'undefined') return 0;
  if (prefersReducedMotion()) {
    // Пробу переходом здесь гасит правило reduced motion, а база токена записана готовым числом.
    return toMs(rootValue(token.replace('--duration-', '--motion-base-')));
  }
  return probeMs(token);
}
