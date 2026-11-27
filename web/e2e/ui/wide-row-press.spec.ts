import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedPlan } from '../../testing/fixtures/plan';
import { bootSecondary } from './secondary-fixture';
import { expect, test } from './fixtures';

// Попадание указателя при сжатии кнопки jsdom не считает: проверка только в браузере.
test.use({ reducedMotion: 'reduce' });

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const DESKTOP = { width: 1280, height: 800 };

/** Нажатие у правого края строки: цель под указателем в момент зажатия и результат отпускания. */
async function pressRight(page: Page, button: Locator): Promise<boolean> {
  const box = await button.boundingBox();
  if (!box) throw new Error('Край строки не виден');
  const x = box.x + box.width - 3;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  const underPointer = await button.evaluate(
    (element, point) => document.elementFromPoint(point.x, point.y)?.closest('button') === element,
    { x, y }
  );
  await page.mouse.up();
  return underPointer;
}

async function expectToggle(button: Locator, before: string | null): Promise<void> {
  await expect(button).toHaveAttribute('aria-expanded', before === 'true' ? 'false' : 'true');
}

test('заголовок раздела сессий раскрывается нажатием у правого края', async ({ page }) => {
  await page.setViewportSize(DESKTOP);
  await bootSecondary(page, '/sessions');
  const head = page.locator('button.section-head').first();
  await expect(head).toBeVisible({ timeout: 15_000 });
  const before = await head.getAttribute('aria-expanded');
  expect(await pressRight(page, head), 'зажатие уводит строку из-под указателя').toBe(true);
  await expectToggle(head, before);
});

test('заголовок задачи плана раскрывается нажатием у правого края', async ({ page }) => {
  await page.setViewportSize(DESKTOP);
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, { id: 'workstation', name: 'workstation', harnesses: [] });
  const seed = seedPlan(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/plan/${seed.orchestrator}`);
  await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  const head = page.locator('[data-plan-task] button.head').first();
  await expect(head).toBeVisible();
  const before = await head.getAttribute('aria-expanded');
  expect(await pressRight(page, head), 'зажатие уводит строку из-под указателя').toBe(true);
  await expectToggle(head, before);
});
