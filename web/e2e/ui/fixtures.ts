import { test as base, expect } from '@playwright/test';

export const test = base.extend({
  page: async ({ page }, use, testInfo) => {
    const violations: string[] = [];
    await page.addInitScript(() => {
      window.addEventListener('securitypolicyviolation', (event) => {
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

/** Сменить тему из чата: на телефоне переключатель живёт в меню сессии, на рабочем столе в шапке. */
export async function toggleChatTheme(page: import('@playwright/test').Page) {
  const button = page.locator('.chat-header [data-theme-toggle]');
  if (await button.count()) return button.click();
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: /Светлая тема|Тёмная тема/ }).click();
}
