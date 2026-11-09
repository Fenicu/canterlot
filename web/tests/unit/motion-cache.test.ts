import { readFileSync } from 'node:fs';
import postcss from 'postcss';
import { afterEach, expect, test, vi } from 'vitest';
import { durationMs, fadeMs, motionScale, prefersReducedMotion, scaledDurationMs, setMotionScale } from '../../src/lib/motion';

const BASE: Record<string, number> = { '--duration-fast': 160, '--duration-expand': 240 };

/** Окружение, где токены считаются как в tokens.css: база на `--motion-scale` корня, системное «меньше движения» гасит всё. */
function setup() {
  const reduced = { matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() };
  vi.stubGlobal('matchMedia', () => reduced);
  const scale = (): string => (reduced.matches ? '0' : document.documentElement.style.getPropertyValue('--motion-scale') || '1');
  const reads = vi.fn((node: Element) => {
    const token = /var\((--[a-z-]+)\)/.exec((node as HTMLElement).style.transitionDuration)?.[1] ?? '';
    return {
      transitionDuration: `${(BASE[token] ?? 0) * Number(scale())}ms`,
      getPropertyValue: (name: string) => (name === '--motion-scale' ? scale() : name.startsWith('--motion-base-') ? `${BASE[name.replace('--motion-base-', '--duration-')]}ms` : '')
    } as unknown as CSSStyleDeclaration;
  });
  vi.stubGlobal('getComputedStyle', reads);
  const appends = vi.spyOn(document.documentElement, 'append');
  return { reduced, reads, appends };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.documentElement.removeAttribute('style');
  document.documentElement.removeAttribute('data-motion');
  document.documentElement.removeAttribute('data-theme');
  document.documentElement.removeAttribute('data-palette');
  document.head.querySelectorAll('style[data-test]').forEach((node) => node.remove());
});

test('mounting_rows_reads_motion_values_once_per_document_state', () => {
  const { reads, appends } = setup();
  expect(durationMs('--duration-fast')).toBe(160);
  expect(prefersReducedMotion()).toBe(false);
  expect(motionScale()).toBe(1);
  const style = reads.mock.calls.length;
  const probes = appends.mock.calls.length;
  for (let row = 0; row < 100; row++) {
    expect(prefersReducedMotion()).toBe(false);
    expect(motionScale()).toBe(1);
    expect(durationMs('--duration-fast')).toBe(160);
  }
  expect(reads.mock.calls.length).toBe(style);
  expect(appends.mock.calls.length).toBe(probes);
  // Смена темы и палитры длительностей не трогает: снимок остаётся.
  document.documentElement.dataset.theme = 'light';
  document.documentElement.dataset.palette = 'nord';
  expect(durationMs('--duration-fast')).toBe(160);
  expect(reads.mock.calls.length).toBe(style);
});

test('reduced_motion_change_invalidates_the_snapshot_at_once', () => {
  const { reduced } = setup();
  expect(durationMs('--duration-expand')).toBe(240);
  expect(scaledDurationMs(100)).toBe(100);
  reduced.matches = true;
  expect(prefersReducedMotion()).toBe(true);
  expect(durationMs('--duration-expand')).toBe(0);
  expect(scaledDurationMs(100)).toBe(0);
  // Прозрачность при выключенном движении идёт с базой токена.
  expect(fadeMs('--duration-fast')).toBe(160);
  reduced.matches = false;
  expect(prefersReducedMotion()).toBe(false);
  expect(durationMs('--duration-expand')).toBe(240);
});

test('motion_scale_change_invalidates_the_snapshot_at_once', () => {
  setup();
  expect(durationMs('--duration-fast')).toBe(160);
  setMotionScale(1.5);
  expect(motionScale()).toBe(1.5);
  expect(durationMs('--duration-fast')).toBe(240);
  setMotionScale(0.5);
  expect(durationMs('--duration-fast')).toBe(80);
  setMotionScale(0);
  expect(prefersReducedMotion()).toBe(true);
  expect(durationMs('--duration-fast')).toBe(0);
  // Свой стиль корня, записанный мимо setMotionScale, тоже сбрасывает снимок.
  document.documentElement.style.setProperty('--motion-scale', '8');
  expect(durationMs('--duration-fast')).toBe(1280);
  document.documentElement.removeAttribute('style');
  expect(durationMs('--duration-fast')).toBe(160);
});

test('late_stylesheet_invalidates_the_snapshot', () => {
  const { reads } = setup();
  durationMs('--duration-fast');
  const before = reads.mock.calls.length;
  const sheet = document.createElement('style');
  sheet.dataset.test = '';
  sheet.textContent = ':root { --x: 1; }';
  document.head.append(sheet);
  durationMs('--duration-fast');
  expect(reads.mock.calls.length).toBeGreaterThan(before);
});

test('duration_tokens_live_only_in_root', () => {
  // Снимок не следит за темой и палитрой: это верно, пока ни один их селектор не задаёт длительность.
  const text = readFileSync('src/lib/styles/tokens.css', 'utf8');
  const outside: string[] = [];
  postcss.parse(text).walkDecls(/^--(duration|motion-base|motion-scale)/, (decl) => {
    const rule = decl.parent;
    if (rule?.type === 'rule' && (rule as postcss.Rule).selector.trim() !== ':root') outside.push(`${(rule as postcss.Rule).selector} ${decl.prop}`);
  });
  expect(outside).toEqual([]);
});
