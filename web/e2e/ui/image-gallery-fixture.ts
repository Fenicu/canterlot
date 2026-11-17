import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';
import { Fingers, screenshotPng, settled, viewer } from './image-viewer-fixture';
import { expect } from './fixtures';

/** Поддельный хаб для снимков и тестов общего просмотрщика: картинки всей сессии из разных мест ленты. */

const OWNER = '00000000-0000-4000-8000-0000000000f1';
const AT = Date.parse('2026-11-02T12:00:00.000Z');

export const phone = (name: string) => name === 'phone';

export interface GallerySeed {
  hub: FakeHub;
  api: FakeHubApi;
  id: string;
  /** id вводов владельца. */
  inputs: string[];
}



function ownerInput(api: FakeHubApi, n: number, image: string): HistoryInput {
  const attachment = { attachment_id: image, name: `shot-${n}.png`, mime: 'image/png', size: 40_000, sha256: '', image: true };
  return {
    kind: 'input',
    id: api.id(),
    idempotency_key: `input-${n}`,
    state: 'accepted',
    content: [{ type: 'text', text: `Сообщение ${n}` }, { type: 'attachment', ...attachment }],
    attachments: [attachment],
    created_seq: String(n * 2),
    feed_seq: String(n * 2),
    created_at: new Date(AT + n * 60_000).toISOString()
  };
}

/** Картинка, на которую смотрел агент: `/api/v1/media/{id}`. */
function mediaBlock(api: FakeHubApi, n: number, image: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: '', rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'text',
    content: '',
    content_bytes: 0,
    content_truncated: false,
    created_seq: String(n * 2 + 1),
    created_at: new Date(AT + n * 60_000 + 20_000).toISOString(),
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    media: [{ media_id: image, mime: 'image/png', width: 640, height: 400, ready: true }]
  };
}

/** Файл, который агент отдал владельцу: `/api/v1/attachments/{id}/content`. */
function fileBlock(api: FakeHubApi, n: number, image: string): HistoryBlock {
  const id = api.id();
  api.blocks.set(id, { text: '', rev: 1, state: 'final' });
  return {
    kind: 'block',
    id,
    block_kind: 'tool_call',
    tool_name: 'share_file',
    summary: 'Снимки',
    content: '',
    content_bytes: 0,
    content_truncated: false,
    created_seq: String(n * 2 + 1),
    created_at: new Date(AT + n * 60_000 + 30_000).toISOString(),
    rev: 1,
    state: 'final',
    agent: 'агент',
    model: 'opus',
    files: [{ attachment_id: image, name: `share-${n}.png`, mime: 'image/png', size: 30_000, title: `Снимок ${n}`, note: '', width: 640, height: 400 }]
  };
}

async function servePictures(page: Page): Promise<void> {
  const serve = async (route: import('@playwright/test').Route, image: string | undefined, preview: boolean) => {
    if (!image) return route.fallback();
    const n = Number(image.split('-').at(-1)) || 1;
    const body = preview ? screenshotPng(160, 100, (n * 37) % 360) : screenshotPng(640, 400, (n * 37) % 360);
    await route.fulfill({
      contentType: 'image/png',
      body,
      headers: {
        'Cache-Control': 'private, max-age=86400, immutable',
        ETag: preview ? `"${image}-preview1"` : `"${image}"`
      }
    });
  };
  await page.route('**/api/v1/attachments/*/content', (route) => {
    const image = route.request().url().split('/attachments/')[1]?.split('/')[0];
    return serve(route, image, false);
  });
  await page.route('**/api/v1/attachments/*/preview', (route) => {
    const image = route.request().url().split('/attachments/')[1]?.split('/')[0];
    return serve(route, image, true);
  });
  await page.route('**/api/v1/media/*', (route) => {
    const image = route.request().url().split('/media/')[1]?.split('?')[0];
    return serve(route, image, false);
  });
  await page.route('**/api/v1/media/*/preview', (route) => {
    const image = route.request().url().split('/media/')[1]?.split('/')[0];
    return serve(route, image, true);
  });
}

/** Пять картинок: сообщение, сообщение, картинка блока, файл блока, сообщение. */
export const FOUR = {
  first: '/api/v1/attachments/g-1/content',
  second: '/api/v1/attachments/g-2/content',
  media: '/api/v1/media/g-3',
  file: '/api/v1/attachments/g-4/content',
  last: '/api/v1/attachments/g-5/content'
};

export async function seedFour(page: Page): Promise<GallerySeed> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const first = ownerInput(api, 1, 'g-1');
  const second = ownerInput(api, 2, 'g-2');
  const media = mediaBlock(api, 3, 'g-3');
  const file = fileBlock(api, 4, 'g-4');
  const last = ownerInput(api, 5, 'g-5');
  api.histories.set(id, [first, second, media, file, last]);
  await installChatRoutes(page, hub, api, OWNER);
  await servePictures(page);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 60_000 });
  return { hub, api, id, inputs: [first.id, second.id, last.id] };
}

/** 130 сообщений с картинкой: лента держит только хвост, просмотрщик — всю сессию. */
export async function seedMany(page: Page, count = 130): Promise<GallerySeed> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  const inputs = Array.from({ length: count }, (_, i) => ownerInput(api, i + 1, `g-${i + 1}`));
  api.histories.set(id, inputs);
  await installChatRoutes(page, hub, api, OWNER);
  await servePictures(page);
  return { hub, api, id, inputs: inputs.map((entry) => entry.id) };
}

/** Миниатюра по адресу файла: у вложения и файла один путь, у картинки блока другой. */
export function thumb(page: Page, href: string) {
  return page.locator(`a.thumb[href="${href}"]`);
}

export async function openViewer(page: Page, button: ReturnType<Page['locator']>) {
  await button.click();
  const dialog = viewer(page);
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible({ timeout: 60_000 });
  await settled(page);
  return dialog;
}

export async function swipe(page: Page, isPhone: boolean, direction: 'older' | 'newer'): Promise<void> {
  if (isPhone) {
    const box = page.viewportSize()!;
    const y = box.height / 2;
    // Палец влево листает вперёд, к новым картинкам.
    const from = { x: direction === 'older' ? box.width * 0.2 : box.width * 0.8, y };
    const to = { x: direction === 'older' ? box.width * 0.8 : box.width * 0.2, y };
    await (await Fingers.on(page)).drag(from, to, 10);
  } else {
    await page.keyboard.press(direction === 'older' ? 'ArrowLeft' : 'ArrowRight');
  }
  await settled(page);
}

/** Кнопки прячутся после простоя: движение мыши возвращает их, как у живого владельца. */
export async function showChrome(page: Page): Promise<void> {
  const box = (await viewer(page).boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await expect(viewer(page)).toHaveAttribute('data-chrome', 'shown');
}
