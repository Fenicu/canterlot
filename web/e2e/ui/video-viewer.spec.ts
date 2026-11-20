import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import type { HistoryInput } from '../../src/lib/stores/chat.svelte';
import { expect, test } from './fixtures';
import { AT, OWNER, block, screenshotPng, settled, viewer } from './image-viewer-fixture';

// Ролик во вложении открывается в просмотрщике плеером браузера, а не скачивается.
// Фальшивый хаб отдаёт настоящий крошечный WebM и отвечает на `Range` кодом 206, как хаб.

const CLIP = readFileSync(resolve(import.meta.dirname, '../../testing/fixtures/clip.webm'));
const CLIP_ID = 'clip-a';
const PICTURE_ID = 'shot-a';
const BROKEN_ID = 'clip-broken';

type Where = 'owner' | 'shared' | 'tool';

interface Opened {
  api: FakeHubApi;
}

/** Чат с роликом (и, если нужно, картинкой рядом) в сообщении владельца, карточке `share_file` или блоке инструмента. */
async function openChat(page: Page, where: Where, files: { id: string; name: string; mime: string; image: boolean; bytes: Buffer }[]): Promise<Opened> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api);
  for (const file of files) api.blobs.set(file.id, { bytes: file.bytes, mime: file.mime });
  if (where === 'owner') {
    const input: HistoryInput = {
      kind: 'input', id: api.id(), idempotency_key: 'clips', state: 'accepted',
      content: [{ type: 'text', text: 'Посмотри запись' }],
      attachments: files.map((file) => ({ attachment_id: file.id, name: file.name, mime: file.mime, size: file.bytes.length, sha256: '', image: file.image })),
      created_seq: '1', feed_seq: '1', created_at: AT
    };
    api.histories.set(id, [input]);
  } else {
    const item = block(api, { tool_name: where === 'shared' ? 'share_file' : 'Write', summary: 'Запись' });
    item.files = files.map((file) => ({ attachment_id: file.id, name: file.name, mime: file.mime, size: file.bytes.length, title: file.name, note: '' }));
    api.histories.set(id, [item]);
  }
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  return { api };
}

const CLIP_FILE = { id: CLIP_ID, name: 'clip.webm', mime: 'video/webm', image: false, bytes: CLIP };
const PICTURE_FILE = { id: PICTURE_ID, name: 'screen.png', mime: 'image/png', image: true, bytes: screenshotPng(640, 400, 210) };

function tile(page: Page, where: Where) {
  return (where === 'owner' ? page.locator('.bubble') : page.locator('.files')).locator('a.tile[data-kind="video"]').first();
}

async function playerLoaded(page: Page) {
  const video = viewer(page).locator('.slide[data-current] video');
  await expect(video).toBeVisible();
  // Кадры ролика доехали: плеер знает размер.
  await expect.poll(() => video.evaluate((node: HTMLVideoElement) => node.videoWidth), { timeout: 15_000 }).toBe(160);
  return video;
}

for (const where of ['owner', 'shared', 'tool'] as const) {
  test(`video_tile_in_${where}_opens_the_player_on_the_media_url_without_autoplay`, async ({ page }) => {
    const { api } = await openChat(page, where, [CLIP_FILE, PICTURE_FILE]);
    await tile(page, where).click();
    const dialog = viewer(page);
    await expect(dialog).toBeVisible();
    const video = await playerLoaded(page);
    await expect(video).toHaveAttribute('src', `/api/v1/attachments/${CLIP_ID}/content`);
    await expect(video).toHaveAttribute('controls', '');
    await expect(video).toHaveAttribute('playsinline', '');
    await expect(video).toHaveAttribute('preload', 'metadata');
    // Ролик начинается только по касанию на «играть».
    expect(await video.evaluate((node: HTMLVideoElement) => ({ autoplay: node.autoplay, paused: node.paused, time: node.currentTime }))).toEqual({ autoplay: false, paused: true, time: 0 });
    // Плеер берёт ролик диапазонами, и хаб отвечает на них 206.
    await expect.poll(() => api.blobRequests.some((request) => request.id === CLIP_ID && request.range !== null && request.status === 206)).toBe(true);
    // «Сохранить» остаётся в шапке для ролика.
    await expect(dialog.locator('a.save')).toHaveAttribute('href', `/api/v1/attachments/${CLIP_ID}/content`);
    await expect(dialog.locator('a.save')).toHaveAttribute('download', 'clip.webm');
    // Ролик и картинка одного сообщения листаются в одном просмотрщике.
    await expect(dialog.locator('.counter')).toHaveText('1 из 2');
    await page.keyboard.press('ArrowRight');
    await expect(dialog.locator('.counter')).toHaveText('2 из 2');
    await expect(dialog.locator('.slide[data-current] .frame')).toBeVisible();
    await expect(dialog.locator('.slide[data-current] video')).toHaveCount(0);
    await page.keyboard.press('ArrowLeft');
    await expect(dialog.locator('.counter')).toHaveText('1 из 2');
    await expect(dialog.locator('.slide[data-current] video')).toBeVisible();
    await settled(page);
    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(tile(page, where)).toBeFocused();
  });
}

test('video_alone_in_a_message_opens_as_a_single_item', async ({ page }) => {
  await openChat(page, 'owner', [CLIP_FILE]);
  await tile(page, 'owner').click();
  const dialog = viewer(page);
  await playerLoaded(page);
  await expect(dialog.locator('.counter')).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: 'Показать в чате' })).toHaveCount(0);
  await expect(dialog.locator('.caption')).toContainText('clip.webm');
});

test('the_player_takes_its_own_clicks_and_keeps_the_browser_menu', async ({ page }) => {
  await openChat(page, 'owner', [CLIP_FILE]);
  // Ссылка на файл остаётся: долгое нажатие и правая кнопка дают меню браузера, как у картинок.
  const link = tile(page, 'owner');
  await expect(link).toHaveAttribute('href', `/api/v1/attachments/${CLIP_ID}/content`);
  expect(await link.evaluate((node) => { const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }); node.dispatchEvent(event); return event.defaultPrevented; })).toBe(false);
  await link.click();
  const video = await playerLoaded(page);
  expect(await video.evaluate((node) => { const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }); node.dispatchEvent(event); return event.defaultPrevented; })).toBe(false);
  // Нажатие на сам плеер доходит до него: сцена не забирает указатель и не прячет кнопки по двойному касанию.
  const target = (await video.boundingBox())!;
  await page.mouse.click(target.x + target.width / 2, target.y + target.height / 2);
  await expect(viewer(page)).toBeVisible();
  await expect(video).toBeVisible();
});

test('unplayable_type_shows_the_fallback_with_the_download_button', async ({ page }) => {
  await openChat(page, 'owner', [{ id: BROKEN_ID, name: 'strange.vid', mime: 'video/x-unplayable', image: false, bytes: Buffer.from('not a movie') }]);
  await tile(page, 'owner').click();
  const dialog = viewer(page);
  await expect(dialog.getByText('Браузер не может воспроизвести это видео')).toBeVisible();
  await expect(dialog.locator('video')).toHaveCount(0);
  const download = dialog.locator('.unplayable a.control');
  await expect(download).toHaveText('Скачать');
  await expect(download).toHaveAttribute('href', `/api/v1/attachments/${BROKEN_ID}/content`);
  await expect(download).toHaveAttribute('download', 'strange.vid');
  // Шапка тоже даёт сохранить.
  await expect(dialog.locator('a.save')).toHaveAttribute('download', 'strange.vid');
});

test('a_player_error_swaps_the_player_for_the_fallback', async ({ page }) => {
  // Тип знаком браузеру, но содержимое не ролик: событие error вместо кадров.
  await openChat(page, 'owner', [{ id: BROKEN_ID, name: 'broken.webm', mime: 'video/webm', image: false, bytes: Buffer.from('this is not a webm file at all, only text') }]);
  await tile(page, 'owner').click();
  const dialog = viewer(page);
  await expect(dialog.getByText('Браузер не может воспроизвести это видео')).toBeVisible({ timeout: 15_000 });
  await expect(dialog.locator('video')).toHaveCount(0);
  await expect(dialog.locator('.unplayable a.control')).toHaveAttribute('download', 'broken.webm');
});

test('reduced_motion_never_starts_the_video_by_itself', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await openChat(page, 'owner', [CLIP_FILE]);
  await tile(page, 'owner').click();
  const video = await playerLoaded(page);
  await page.waitForTimeout(800);
  expect(await video.evaluate((node: HTMLVideoElement) => ({ paused: node.paused, time: node.currentTime }))).toEqual({ paused: true, time: 0 });
});

test('fake_hub_answers_a_range_request_with_206_like_the_hub', async ({ page }) => {
  const { api } = await openChat(page, 'owner', [CLIP_FILE]);
  const probe = (range: string) =>
    page.evaluate(
      async ({ url, range }) => {
        const response = await fetch(url, { headers: { Range: range } });
        return { status: response.status, range: response.headers.get('content-range'), accept: response.headers.get('accept-ranges'), length: (await response.arrayBuffer()).byteLength };
      },
      { url: `/api/v1/attachments/${CLIP_ID}/content`, range }
    );
  expect(await probe('bytes=0-9')).toEqual({ status: 206, range: `bytes 0-9/${CLIP.length}`, accept: 'bytes', length: 10 });
  expect(await probe('bytes=-4')).toEqual({ status: 206, range: `bytes ${CLIP.length - 4}-${CLIP.length - 1}/${CLIP.length}`, accept: 'bytes', length: 4 });
  expect(await probe(`bytes=${CLIP.length}-`)).toMatchObject({ status: 416, range: `bytes */${CLIP.length}` });
  expect(await probe('bytes=0-1,4-5')).toMatchObject({ status: 200, range: null, length: CLIP.length });
  expect(api.blobRequests.map((request) => request.status)).toEqual([206, 206, 416, 200]);
});
