import { afterEach, expect, test } from 'vitest';
import { cubicBezier, easing } from '../../../src/lib/motion';

afterEach(() => {
  document.documentElement.style.removeProperty('--ease-row-fold');
});

test('cubic_bezier_matches_the_css_curve_at_known_points', () => {
  const linear = cubicBezier(0, 0, 1, 1);
  for (const t of [0, 0.1, 0.5, 0.9, 1]) expect(linear(t)).toBeCloseTo(t, 4);
  // ease-in-out симметрична: середина в середине, четверть зеркальна трём четвертям.
  const inOut = cubicBezier(0.42, 0, 0.58, 1);
  expect(inOut(0.5)).toBeCloseTo(0.5, 4);
  expect(inOut(0.25) + inOut(0.75)).toBeCloseTo(1, 4);
  // Кривая с быстрым стартом уже к половине времени проходит почти весь путь.
  const out = cubicBezier(0.25, 1, 0.5, 1);
  expect(out(0.5)).toBeGreaterThan(0.9);
  expect(out(0)).toBe(0);
  expect(out(1)).toBe(1);
});

test('easing_reads_the_curve_from_the_token', () => {
  document.documentElement.style.setProperty('--ease-row-fold', 'cubic-bezier(.42, 0, .58, 1)');
  const fn = easing('--ease-row-fold');
  expect(fn(0.5)).toBeCloseTo(0.5, 4);
  expect(fn(0.25)).toBeCloseTo(cubicBezier(0.42, 0, 0.58, 1)(0.25), 6);
});

test('easing_without_a_curve_falls_back_to_linear', () => {
  const fn = easing('--ease-missing');
  expect(fn(0.3)).toBe(0.3);
});
