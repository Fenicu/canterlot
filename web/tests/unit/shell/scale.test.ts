import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import postcss, { type AtRule } from 'postcss';
import { BROWSER_CHROME_PX, SCALE_STEPS, uiScale } from '#lib/shell/layout.js';

// Окна мониторов: высота без 90 px панели браузера, стоя — те же размеры, повёрнутые.
const CHROME = 90;
const MONITORS: [string, number, number, number][] = [
  ['Full HD 100%', 1920, 1080, 1.0667],
  ['Full HD 125%', 1536, 864, 1],
  ['2K 100%', 2560, 1440, 1.2],
  ['2K 125%', 2048, 1152, 1.0667],
  ['2K 150%', 1707, 960, 1],
  ['4K 100%', 3840, 2160, 1.6],
  ['4K 125%', 3072, 1728, 1.2],
  ['4K 150%', 2560, 1440, 1.2],
  ['4K 200%', 1920, 1080, 1.0667]
];

describe('shell/layout: масштаб интерфейса не зависит от поворота', () => {
  it.each(MONITORS)('%s: боком и стоя один шаг', (_, long, short, scale) => {
    expect(uiScale(long, short - CHROME)).toBe(scale);
    expect(uiScale(short, long - CHROME)).toBe(scale);
  });

  it('окно боком считается как сегодня: ширина от 118rem и высота от 56rem', () => {
    expect(uiScale(1888, 896)).toBe(1.0667);
    expect(uiScale(1887, 896)).toBe(1);
    expect(uiScale(1888, 895)).toBe(1);
    expect(uiScale(2496, 1120)).toBe(1.2);
    expect(uiScale(3680, 1760)).toBe(1.6);
    expect(uiScale(1871, 880)).toBe(1);
    expect(uiScale(1440, 900)).toBe(1);
  });

  it('окно стоя: панель браузера отнята от длинной стороны, поэтому порог высоты ниже на неё', () => {
    expect(BROWSER_CHROME_PX).toBe(96);
    expect(uiScale(992, 1792)).toBe(1.0667);
    expect(uiScale(991, 1792)).toBe(1);
    expect(uiScale(992, 1791)).toBe(1);
    expect(uiScale(1216, 2400)).toBe(1.2);
    expect(uiScale(1856, 3584)).toBe(1.6);
  });

  it('телефон и планшет остаются на 1', () => {
    for (const [w, h] of [[412, 892], [892, 412], [820, 1180], [1180, 820], [1024, 1366], [1366, 1024]]) {
      expect(uiScale(w!, h!)).toBe(1);
    }
  });

  it('пороги и множители совпадают с медиазапросами tokens.css', () => {
    const queries = new Map<string, string>();
    postcss.parse(readFileSync('src/lib/styles/tokens.css', 'utf8')).walkAtRules('media', (rule: AtRule) => {
      const value = (rule.first as { first?: { prop?: string; value?: string } } | undefined)?.first;
      if (value?.prop === '--ui-scale') queries.set(rule.params, value.value!);
    });
    const rem = (px: number) => `${px / 16}rem`;
    const expected = SCALE_STEPS.map((step) => [
      `(min-width: ${rem(step.long)}) and (min-height: ${rem(step.short - BROWSER_CHROME_PX)}), (min-width: ${rem(step.short)}) and (min-height: ${rem(step.long - BROWSER_CHROME_PX)})`,
      String(step.scale)
    ]);
    expect([...queries.entries()]).toEqual(expected);
  });
});
