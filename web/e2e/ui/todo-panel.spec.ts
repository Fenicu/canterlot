import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

// Раскладку и попадание указателя jsdom не считает: проверка только в браузере.
test.use({ reducedMotion: 'reduce' });

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function bootChat(page: Page, viewport: { width: number; height: number }): Promise<Locator> {
  await page.setViewportSize(viewport);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, { id: 'workstation', name: 'workstation', harnesses: [] });
  const id = seedConversation(api, { title: 'Проверка строк задач' });
  await installChatRoutes(page, hub, api, OWNER);
  await page.addInitScript(() => localStorage.removeItem('canterlot.todo-open'));
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  api.pushTodo(id, [
    { status: 'completed', text: 'Собрать данные' },
    { status: 'in_progress', text: 'Проверить отчёт' },
    { status: 'pending', text: 'Отправить' }
  ]);
  hub.flush();
  const strip = page.locator('section.todo button.slim').first();
  await expect(strip).toBeVisible({ timeout: 15_000 });
  await expect(strip).toHaveAttribute('aria-expanded', 'false');
  return strip;
}

/** Нажатие на стрелку: проверяем цель под указателем в момент зажатия и результат отпускания. */
async function pressChevron(page: Page, strip: Locator): Promise<boolean> {
  const box = await strip.locator('.chevron').boundingBox();
  if (!box) throw new Error('Стрелка строки задач не видна');
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  const underPointer = await page.evaluate(
    ({ x, y }) => Boolean(document.elementFromPoint(x, y)?.closest('button.slim')),
    { x, y }
  );
  await page.mouse.up();
  return underPointer;
}

for (const [name, viewport] of [
  ['широкий экран', { width: 1280, height: 800 }],
  ['телефон', { width: 390, height: 844 }]
] as const) {
  test(`стрелка строки задач раскрывает и свёртывает список: ${name}`, async ({ page }) => {
    const strip = await bootChat(page, viewport);
    expect(await pressChevron(page, strip), 'зажатие уводит строку из-под указателя').toBe(true);
    await expect(strip).toHaveAttribute('aria-expanded', 'true');
    expect(await pressChevron(page, strip), 'зажатие уводит строку из-под указателя').toBe(true);
    await expect(strip).toHaveAttribute('aria-expanded', 'false');
  });
}
