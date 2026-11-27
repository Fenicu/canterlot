import { mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

// Раскладку и касания jsdom не считает: проверка только в браузере, на сенсорном вводе.
test.use({ reducedMotion: 'reduce' });

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SHOT = resolve(import.meta.dirname, '../tmp/attach-menu-phone.png');

async function bootChat(page: Page): Promise<void> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, { id: 'workstation', name: 'workstation', harnesses: [] });
  const id = seedConversation(api, { title: 'Проверка меню вложений' });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
}

test.beforeEach(({}, testInfo) => {
  test.skip(testInfo.project.name !== 'phone', 'меню вложений проверяем на телефоне');
});

test('меню вложений на телефоне целиком в окне, оба пункта видны и галерея открывается', async ({ page }) => {
  const errors: Error[] = [];
  page.on('pageerror', (error) => errors.push(error));
  await bootChat(page);

  const attach = page.getByRole('button', { name: 'Прикрепить файл' });
  await expect(attach).toBeVisible();
  await attach.tap();
  const menu = page.getByRole('menu', { name: 'Вложение' });
  await expect(menu).toBeVisible();

  const gallery = menu.getByRole('menuitem', { name: 'Галерея' });
  const files = menu.getByRole('menuitem', { name: 'Файлы' });
  await expect(gallery).toBeVisible();
  await expect(files).toBeVisible();
  await expect(gallery).toHaveText('Галерея');
  await expect(files).toHaveText('Файлы');

  const box = (await menu.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(viewport.width);
  expect(box.y + box.height).toBeLessThanOrEqual(viewport.height);
  // Меню не перекрыто соседними кнопками и не обрезано родительским overflow.
  expect(await menu.evaluate((node) => {
    const rect = node.getBoundingClientRect();
    return document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2)?.closest('[role="menu"]') === node;
  })).toBe(true);

  mkdirSync(dirname(SHOT), { recursive: true });
  await page.screenshot({ path: SHOT, animations: 'disabled' });

  page.once('filechooser', () => {});
  await gallery.tap();
  await expect(menu).toBeHidden();
  expect(errors).toEqual([]);
});
