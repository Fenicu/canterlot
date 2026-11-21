import type { Page } from '@playwright/test';

/** Медленный переход, чтобы застать движение на месте: масштаб растягивает все длительности. */
export async function slowMotion(page: Page, scale = 8): Promise<void> {
  await page.evaluate((value) => document.documentElement.style.setProperty('--motion-scale', String(value)), scale);
}

/** Масштаб движения 0: токены длительности дают нуль, как при выключенном движении в настройках. */
export async function noMotion(page: Page): Promise<void> {
  await page.evaluate(() => document.documentElement.style.setProperty('--motion-scale', '0'));
}

/** Сколько узлов по селектору складываются или раскладываются сейчас: у настоящей складки меняется высота. */
export async function foldingCount(page: Page, selector: string): Promise<number> {
  return page.evaluate((query) => [...document.querySelectorAll(query)].filter((node) => node.getAnimations().some((animation) => {
    const effect = animation.effect;
    return effect instanceof KeyframeEffect && Number(effect.getComputedTiming().duration) > 0 && effect.getKeyframes().some((frame) => 'height' in frame);
  })).length, selector);
}

/** Сколько анимаций с ненулевой длительностью идёт на узлах по селектору и внутри них. */
export async function movingCount(page: Page, selector: string): Promise<number> {
  return page.evaluate((query) => [...document.querySelectorAll(query)].reduce((sum, node) => sum + node.getAnimations({ subtree: true }).filter((animation) => {
    const effect = animation.effect;
    return effect instanceof KeyframeEffect && Number(effect.getComputedTiming().duration) > 0;
  }).length, 0), selector);
}
