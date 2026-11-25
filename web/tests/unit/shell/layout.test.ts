import { describe, expect, it } from 'vitest';
import { PORTRAIT_DESK_MIN_HEIGHT, PORTRAIT_DESK_MIN_WIDTH, dockForm, layoutMode } from '#lib/shell/layout.js';

describe('shell/layout: режим раскладки', () => {
  const mouse = (width: number, height: number) => ({ width, height, mouse: true });
  const touch = (width: number, height: number) => ({ width, height, mouse: false });

  it('по ширине как сегодня: телефон уже 44rem, планшет до 74rem, дальше стол', () => {
    expect(layoutMode(touch(412, 892))).toBe('phone');
    expect(layoutMode(touch(703, 900))).toBe('phone');
    expect(layoutMode(touch(704, 900))).toBe('tablet');
    expect(layoutMode(touch(1183, 900))).toBe('tablet');
    expect(layoutMode(touch(1184, 900))).toBe('desktop');
    expect(layoutMode(mouse(1871, 880))).toBe('desktop');
  });

  it('монитор стоя с мышью — стол с нижней панелью', () => {
    for (const [w, h] of [[1080, 1830], [1152, 1958], [1440, 2470], [1728, 2982], [2160, 3750]]) {
      expect(layoutMode(mouse(w!, h!)), `${w}×${h}`).toBe('desktop');
      expect(dockForm(mouse(w!, h!)), `${w}×${h}`).toBe(true);
    }
  });

  it('узкое окно стоя остаётся планшетом: колонке ленты не хватило бы 600 px', () => {
    expect(PORTRAIT_DESK_MIN_WIDTH).toBe(1066);
    expect(layoutMode(mouse(1065, 1830))).toBe('tablet');
    expect(layoutMode(mouse(1066, 1830))).toBe('desktop');
    expect(layoutMode(mouse(960, 1617))).toBe('tablet');
    expect(layoutMode(mouse(864, 1446))).toBe('tablet');
    expect(dockForm(mouse(960, 1617))).toBe(false);
  });

  it('невысокое окно стоя остаётся как сегодня', () => {
    expect(PORTRAIT_DESK_MIN_HEIGHT).toBe(1440);
    expect(layoutMode(mouse(1100, 1439))).toBe('tablet');
    expect(layoutMode(mouse(1100, 1440))).toBe('desktop');
    expect(dockForm(mouse(1200, 1300))).toBe(false);
    expect(layoutMode(mouse(1200, 1300))).toBe('desktop');
  });

  it('окно боком с мышью — правая панель, нижней нет', () => {
    expect(dockForm(mouse(1920, 990))).toBe(false);
    expect(dockForm(mouse(1830, 1830))).toBe(false);
  });

  it('сенсорный планшет стоя и боком — как сегодня', () => {
    expect(layoutMode(touch(820, 1180))).toBe('tablet');
    expect(layoutMode(touch(1024, 1366))).toBe('tablet');
    expect(layoutMode(touch(1180, 820))).toBe('tablet');
    expect(layoutMode(touch(1366, 1024))).toBe('desktop');
    expect(layoutMode(touch(1080, 1830))).toBe('tablet');
    expect(dockForm(touch(1440, 2470))).toBe(false);
    expect(layoutMode(touch(1440, 2470))).toBe('desktop');
  });
});
