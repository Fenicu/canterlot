import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, resizeViewport, test } from './fixtures';
import { screenshotPng } from './image-viewer-fixture';

// Раскладку и размеры окна jsdom не считает: проверка только в браузере.
test.use({ reducedMotion: 'reduce' });

const OWNER = '00000000-0000-4000-8000-0000000000f1';
const TMP = resolve(import.meta.dirname, '../tmp');
const IMAGES = [
  { name: 'tall', width: 1080, height: 2400, hue: 130 },
  { name: 'wide', width: 1920, height: 1080, hue: 210 }
] as const;

async function bootChat(page: Page): Promise<void> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, { id: 'workstation', name: 'workstation', harnesses: [] });
  const id = seedConversation(api, { title: 'Проверка окна правки' });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
}

/** Картинка сразу ложится в лоток, а редактор открывает карандаш на её миниатюре. */
async function openEditor(page: Page, image: (typeof IMAGES)[number], shot: string): Promise<void> {
  const phone = test.info().project.name === 'phone';
  await page.locator('input.file:not([accept])').setInputFiles({
    name: `${image.name}.png`,
    mimeType: 'image/png',
    buffer: screenshotPng(image.width, image.height, image.hue)
  });
  const dialog = page.locator('dialog.editor');
  const row = page.locator('section.tray li').filter({ hasText: `${image.name}.png` });
  const pencil = row.getByRole('button', { name: 'Редактировать изображение' });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(pencil).toBeVisible({ timeout: 15_000 });
  await expect(dialog).toBeHidden();
  const tap = (await pencil.boundingBox())!;
  expect(Math.min(tap.width, tap.height)).toBeGreaterThanOrEqual(44);
  await page.screenshot({ path: shot, animations: 'disabled' });
  if (phone) await pencil.tap();
  else await pencil.click();
  await expect(dialog).toBeVisible({ timeout: 15_000 });
  await expect(dialog.locator('.stage[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(dialog.getByRole('button', { name: 'Без правок' })).toHaveCount(0);
}

test('редактор — окно с полями по краям, холст внутри и панель инструментов сверху', async ({ page }) => {
  const phone = test.info().project.name === 'phone';
  const errors: Error[] = [];
  page.on('pageerror', (error) => errors.push(error));
  await bootChat(page);
  if (!phone) await resizeViewport(page, { width: 1280, height: 800 });
  const viewport = page.viewportSize()!;
  mkdirSync(TMP, { recursive: true });

  for (const image of IMAGES) {
    await openEditor(page, image, resolve(TMP, `image-editor-${phone ? 'phone' : 'desktop'}-${image.name}-tray.png`));
    const dialog = page.locator('dialog.editor');
    const box = (await dialog.boundingBox())!;
    const canvas = (await dialog.locator('canvas').boundingBox())!;

    // Окно не касается краёв экрана: у телефона поля заданы токенами --s12/--s15.
    const margin = 8;
    expect(box.x).toBeGreaterThanOrEqual(margin);
    expect(box.y).toBeGreaterThanOrEqual(margin);
    expect(box.x + box.width).toBeLessThanOrEqual(viewport.width - margin);
    expect(box.y + box.height).toBeLessThanOrEqual(viewport.height - margin);
    // Высокий снимок использует всю доступную высоту, широкий на телефоне даёт низкое окно.
    if (image.name === 'tall') expect(box.height).toBeGreaterThan(viewport.height * 0.7);
    if (phone && image.name === 'wide') expect(box.height).toBeLessThan(viewport.height * 0.7);
    expect(canvas.x).toBeGreaterThanOrEqual(box.x);
    expect(canvas.y).toBeGreaterThanOrEqual(box.y);
    expect(canvas.x + canvas.width).toBeLessThanOrEqual(box.x + box.width);
    expect(canvas.y + canvas.height).toBeLessThanOrEqual(box.y + box.height);
    // Все четыре угловые ручки целиком попадают в видимую (в том числе скруглённую) сцену.
    const corners = await dialog.locator('.stage').evaluate((node) => {
      const [x = 0, y = 0, width = 0, height = 0] = (node.getAttribute('data-frame') ?? '').split(',').map(Number);
      const radius = parseFloat(getComputedStyle(node).borderTopLeftRadius) || 0;
      const stageWidth = node.clientWidth;
      const stageHeight = node.clientHeight;
      const half = 5;
      const points: [number, number][] = [[x, y], [x + width, y], [x, y + height], [x + width, y + height]];
      return points.map(([cx, cy]) => {
        const px = cx < stageWidth / 2 ? cx - half : cx + half;
        const py = cy < stageHeight / 2 ? cy - half : cy + half;
        const dx = px < radius ? radius - px : px > stageWidth - radius ? px - (stageWidth - radius) : 0;
        const dy = py < radius ? radius - py : py > stageHeight - radius ? py - (stageHeight - radius) : 0;
        return px >= 0 && py >= 0 && px <= stageWidth && py <= stageHeight &&
          (dx === 0 || dy === 0 || Math.hypot(dx, dy) <= radius);
      });
    });
    expect(corners).toEqual([true, true, true, true]);
    expect(await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight)).toBe(true);

    await expect(dialog.getByRole('button', { name: 'Обрезать' })).toHaveAttribute('title', 'Обрезать');
    await expect(dialog.getByRole('button', { name: 'Карандаш' })).toHaveAttribute('title', 'Карандаш');
    await expect(dialog.getByRole('button', { name: 'Отменить действие' })).toHaveAttribute('title', 'Отменить действие');
    await expect(dialog.getByRole('button', { name: 'Обрезать' })).toHaveAttribute('aria-pressed', 'true');

    const shot = resolve(TMP, `image-editor-${phone ? 'phone' : 'desktop'}-${image.name}.png`);
    await page.screenshot({ path: shot, animations: 'disabled' });

    if (image.name === 'tall') {
      // Случайное касание затемнения не должно закрывать окно и терять рисунок.
      await page.mouse.click(2, 2);
      expect(await dialog.evaluate((node) => (node as HTMLDialogElement).open)).toBe(true);
    }
    // Высокий снимок закрывается кнопкой «Отмена», широкий — клавишей Escape.
    if (image.name === 'wide') await page.keyboard.press('Escape');
    else if (phone) await dialog.getByRole('button', { name: 'Отмена' }).tap();
    else await dialog.getByRole('button', { name: 'Отмена' }).click();
    await expect(dialog).toBeHidden();
    // «Отмена» оставляет вложение в лотке как было.
    await expect(page.locator('section.tray li').filter({ hasText: `${image.name}.png` })).toHaveCount(1);
  }
  await expect(page.locator('section.tray li')).toHaveCount(IMAGES.length);
  expect(errors).toEqual([]);
});

test('компактный лоток при открытой клавиатуре — карандаш отдельной кнопкой и не задевает крестик', async ({ page }) => {
  const phone = test.info().project.name === 'phone';
  await bootChat(page);
  await page.locator('input.file:not([accept])').setInputFiles([
    ...IMAGES.map((image) => ({ name: `${image.name}.png`, mimeType: 'image/png', buffer: screenshotPng(image.width, image.height, image.hue) })),
    { name: 'report.pdf', mimeType: 'application/pdf', buffer: Buffer.from('%PDF-1.4 mock') }
  ]);
  const rows = page.locator('section.tray li');
  await expect(rows).toHaveCount(3, { timeout: 15_000 });
  for (const image of IMAGES)
    await expect(rows.filter({ hasText: `${image.name}.png` }).getByRole('button', { name: 'Редактировать изображение' })).toBeVisible({ timeout: 15_000 });

  // Клавиатура съедает высоту: ниже 760 px поле ввода переходит в компактный вид.
  await page.setViewportSize(phone ? { width: 412, height: 420 } : { width: 1280, height: 600 });
  await expect(page.locator('html[data-compact]')).toHaveCount(1);
  await page.locator('textarea').focus();
  mkdirSync(TMP, { recursive: true });
  await page.screenshot({ path: resolve(TMP, `image-editor-${phone ? 'phone' : 'desktop'}-compact.png`), animations: 'disabled' });

  const overlap = (a: { x: number; y: number; width: number; height: number }, b: typeof a) =>
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
  for (const name of ['tall.png', 'wide.png', 'report.pdf']) {
    const row = rows.filter({ hasText: name });
    await row.scrollIntoViewIfNeeded();
    const close = (await row.getByRole('button', { name: 'Убрать' }).boundingBox())!;
    const tile = (await row.locator('.tile').boundingBox())!;
    const label = (await row.locator('.name').boundingBox())!;
    expect(Math.min(close.width, close.height)).toBeGreaterThanOrEqual(44);
    const pencils = row.getByRole('button', { name: 'Редактировать изображение' });
    if (name === 'report.pdf') {
      await expect(pencils).toHaveCount(0);
      continue;
    }
    await expect(pencils).toBeVisible();
    const pencil = (await pencils.boundingBox())!;
    expect(Math.min(pencil.width, pencil.height)).toBeGreaterThanOrEqual(44);
    expect(overlap(pencil, close)).toBe(false);
    expect(overlap(pencil, tile)).toBe(false);
    expect(overlap(pencil, label)).toBe(false);
  }

  await rows.filter({ hasText: 'tall.png' }).scrollIntoViewIfNeeded();
  const pencil = rows.filter({ hasText: 'tall.png' }).getByRole('button', { name: 'Редактировать изображение' });
  if (phone) await pencil.tap();
  else await pencil.click();
  const dialog = page.locator('dialog.editor');
  await expect(dialog).toBeVisible({ timeout: 15_000 });
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(rows).toHaveCount(3);
});
