import { test as base, expect } from '@playwright/test';
import { layoutMode } from '../../src/lib/shell/layout';

export const test = base.extend({
  page: async ({ page }, use, testInfo) => {
    const violations: string[] = [];
    await page.addInitScript(() => {
      window.addEventListener('securitypolicyviolation', (event) => {
        // axe-core для части проверок вставляет свой <style>: это нарушение самого сканера, не приложения.
        if (event.violatedDirective.startsWith('style-src') && event.blockedURI === 'inline' && 'axe' in window) return;
        const list = ((window as unknown as { __csp?: string[] }).__csp ??= []);
        list.push(`${event.violatedDirective} ${event.blockedURI}`);
      });
    });
    await use(page);
    if (page.isClosed()) return;
    const found = await page.evaluate(() => (window as unknown as { __csp?: string[] }).__csp ?? []).catch(() => []);
    if (found.length > 0) {
      violations.push(...found);
      testInfo.attach('csp', { body: violations.join('\n'), contentType: 'text/plain' });
    }
    expect(violations).toEqual([]);
  }
});

export { expect };

/**
 * Заполняет рабочую папку и гасит список подсказок: он лежит поверх формы,
 * и без Escape следующее нажатие попадает в вариант списка, а не в контрол.
 */
export async function fillWorkdir(page: import('@playwright/test').Page, path: string) {
  const field = page.getByRole('textbox', { name: 'Рабочая папка' });
  await field.fill(path);
  await field.press('Escape');
  await expect(page.getByRole('listbox', { name: 'Подсказки папок' })).toBeHidden();
}

/** Сменить тему из чата: на телефоне переключатель живёт в меню сессии, на рабочем столе в шапке. */
export async function toggleChatTheme(page: import('@playwright/test').Page) {
  const button = page.locator('.chat-header [data-theme-toggle]');
  // Переключатель темы и меню сессии рисуются одной шапкой: пока меню нет, отсутствие переключателя ничего не значит.
  await expect(page.locator('.chat-header').getByRole('button', { name: 'Меню сессии' })).toBeVisible();
  if (await button.count()) return button.click();
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: /Светлая тема|Тёмная тема/ }).click();
}

/** Контраст заливки элемента с ближайшим непрозрачным фоном под ним, по формуле WCAG. */
export async function fillContrast(locator: import('@playwright/test').Locator): Promise<number> {
  return locator.evaluate((element) => {
    const rgb = (value: string) => (value.match(/[\d.]+/g) ?? []).map(Number);
    const opaque = (value: string) => { const parts = rgb(value); return parts.length >= 3 && (parts[3] ?? 1) > 0; };
    const luminance = ([r, g, b]: number[]) => [r!, g!, b!].map((c) => { const s = c / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; }).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i]!, 0);
    const own = getComputedStyle(element).backgroundColor;
    let parent = element.parentElement;
    let under = getComputedStyle(document.documentElement).backgroundColor;
    while (parent) { const value = getComputedStyle(parent).backgroundColor; if (opaque(value)) { under = value; break; } parent = parent.parentElement; }
    const [a, b] = [luminance(rgb(own)), luminance(rgb(under))].sort((x, y) => y - x);
    return (a! + 0.05) / (b! + 0.05);
  });
}

/**
 * Сменить размер окна и дождаться, пока оболочка перестроится под него.
 * Раскладку оболочка берёт из события resize, а оно приходит кадром позже:
 * сразу после setViewportSize страница ещё стоит в прежней раскладке.
 */
export async function resizeViewport(page: import('@playwright/test').Page, size: { width: number; height: number }) {
  await page.setViewportSize(size);
  const width = await page.evaluate(() => document.documentElement.clientWidth);
  await expect(page.locator('.shell')).toHaveAttribute('data-layout', layoutMode(width));
}
