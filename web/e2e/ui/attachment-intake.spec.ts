import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const PNG = [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0];

async function boot(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toBeVisible();
  return { hub, api, id };
}

async function pasteImage(page: Page) {
  return page.evaluate((bytes) => {
    const file = new File([new Uint8Array(bytes)], '', { type: 'image/png' });
    const data = new DataTransfer();
    data.items.add(file);
    const event = new ClipboardEvent('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(event, 'clipboardData', { value: data });
    const area = document.querySelector('textarea[aria-label="Сообщение"]');
    area?.dispatchEvent(event);
    return event.defaultPrevented;
  }, PNG);
}

async function pasteMixed(page: Page) {
  return page.evaluate((bytes) => {
    const file = new File([new Uint8Array(bytes)], 'image.png', { type: 'image/png' });
    const data = new DataTransfer();
    data.setData('text/plain', 'подпись к снимку');
    data.items.add(file);
    const event = new ClipboardEvent('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(event, 'clipboardData', { value: data });
    const area = document.querySelector('textarea[aria-label="Сообщение"]');
    area?.dispatchEvent(event);
    return event.defaultPrevented;
  }, PNG);
}

async function startFileDrag(page: Page, name: string) {
  return page.evaluate(
    ({ bytes, name }) => {
      const file = new File([new Uint8Array(bytes)], name, { type: 'image/png' });
      const data = new DataTransfer();
      data.items.add(file);
      (window as unknown as { __canterlotDrop?: DataTransfer }).__canterlotDrop = data;
      const target = document.querySelector('[data-composer]');
      const enter = new DragEvent('dragenter', { bubbles: true, cancelable: true, dataTransfer: data });
      const over = new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: data });
      target?.dispatchEvent(enter);
      target?.dispatchEvent(over);
      return [enter.defaultPrevented, over.defaultPrevented];
    },
    { bytes: PNG, name },
  );
}

async function finishFileDrag(page: Page) {
  return page.evaluate(() => {
    const data = (window as unknown as { __canterlotDrop?: DataTransfer }).__canterlotDrop;
    const target = document.querySelector('[data-composer]');
    const drop = new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: data });
    target?.dispatchEvent(drop);
    return drop.defaultPrevented;
  });
}

function watchCreated(page: Page): string[] {
  const bodies: string[] = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (request.method() === 'POST' && url.pathname === '/api/v1/attachments') {
      bodies.push(request.postData() ?? '');
    }
  });
  return bodies;
}

function screenshotPath(info: { project: { name: string } }): string | null {
  const base = process.env.INTAKE_SCREENSHOT;
  return base ? `${base}-${info.project.name}.png` : null;
}

test('paste_adds_an_image_to_the_tray_and_uploads_it_through_the_hub', async ({ page }, info) => {
  await boot(page);
  const created = watchCreated(page);
  const prevented = await pasteImage(page);
  expect(prevented).toBe(false);

  const tray = page.locator('[data-composer] .tray');
  await expect(tray.getByText(/^Снимок экрана \d{4}-\d{2}-\d{2} \d{2}-\d{2}\.png$/)).toBeVisible();
  await expect(tray.getByText('Загружено')).toHaveCount(1, { timeout: 15_000 });
  await expect.poll(() => created.length).toBe(1);
  const body = JSON.parse(created[0] ?? '{}') as { name?: string; mime?: string };
  expect(body.name).toMatch(/^Снимок экрана \d{4}-\d{2}-\d{2} \d{2}-\d{2}\.png$/);
  expect(body.mime).toBe('image/png');

  const shot = screenshotPath(info);
  if (shot) await page.screenshot({ path: shot });
});

test('mixed_paste_adds_the_image_without_blocking_the_text_paste', async ({ page }) => {
  await boot(page);
  expect(await pasteMixed(page)).toBe(false);
  const tray = page.locator('[data-composer] .tray');
  await expect(tray.getByText(/^Снимок экрана \d{4}-\d{2}-\d{2} \d{2}-\d{2}\.png$/)).toBeVisible();
  await expect(tray.getByText('Загружено')).toHaveCount(1, { timeout: 15_000 });
});

test('drop_shows_a_target_adds_the_file_and_uploads_it', async ({ page }) => {
  await boot(page);
  const created = watchCreated(page);
  const prevented = await startFileDrag(page, '');
  expect(prevented).toEqual([true, true]);
  await expect(page.getByText('Отпустите файлы')).toBeVisible();
  expect(await finishFileDrag(page)).toBe(true);
  await expect(page.getByText('Отпустите файлы')).toHaveCount(0);

  const tray = page.locator('[data-composer] .tray');
  await expect(tray.getByText(/^Снимок экрана \d{4}-\d{2}-\d{2} \d{2}-\d{2}\.png$/)).toBeVisible();
  await expect(tray.getByText('Загружено')).toHaveCount(1, { timeout: 15_000 });
  await expect.poll(() => created.length).toBe(1);
});

test('dropping_files_outside_the_composer_is_blocked_and_adds_nothing', async ({ page }) => {
  await boot(page);
  const before = page.url();
  const prevented = await page.evaluate((bytes) => {
    const file = new File([new Uint8Array(bytes)], 'outside.png', { type: 'image/png' });
    const data = new DataTransfer();
    data.items.add(file);
    const event = new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: data });
    document.body.dispatchEvent(event);
    return event.defaultPrevented;
  }, PNG);
  expect(prevented).toBe(true);
  expect(page.url()).toBe(before);
  await expect(page.locator('[data-composer] .tray')).toHaveCount(0);
});
