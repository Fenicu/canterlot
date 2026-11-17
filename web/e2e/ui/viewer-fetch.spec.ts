import { expect, test } from './fixtures';
import { openChat, settled, thumb, viewer } from './image-viewer-fixture';

// Лента и просмотрщик качают уменьшенный показ, оригинал — один раз на картинку.
test.describe('viewer_fetch', () => {
  test('answer_images_use_previews_and_fetch_full_once_per_image', async ({ page }) => {
    const fetches = new Map<string, number>();
    const cached = new Map<string, string | null>();
    page.on('request', (request) => {
      const path = new URL(request.url()).pathname;
      if (path.startsWith('/api/v1/media/')) fetches.set(path, (fetches.get(path) ?? 0) + 1);
    });
    page.on('response', (response) => {
      const path = new URL(response.url()).pathname;
      if (path.startsWith('/api/v1/media/')) cached.set(path, response.headers()['cache-control'] ?? null);
    });
    await openChat(page, 'answer');
    // Миниатюры ленты — уменьшенный показ, а не оригинал.
    const thumbs = page.locator('.shot a img');
    await expect(thumbs.first()).toBeVisible();
    for (const src of await thumbs.evaluateAll((nodes) => nodes.map((node) => (node as HTMLImageElement).currentSrc))) {
      expect(new URL(src).pathname.endsWith('/preview')).toBe(true);
    }
    await thumb(page, 'answer', 0).click();
    const dialog = viewer(page);
    await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
    await settled(page);
    // Фон текущей картинки — показ, оригинал поверх него запрошен ровно один раз.
    expect(fetches.get('/api/v1/media/shot-a/preview')).toBeGreaterThanOrEqual(1);
    expect(fetches.get('/api/v1/media/shot-a')).toBe(1);
    // Соседи заранее тянут только показ.
    expect(fetches.get('/api/v1/media/shot-b')).toBeUndefined();
    await page.keyboard.press('ArrowRight');
    await expect(dialog.locator('.counter')).toHaveText('2 из 3');
    await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
    await settled(page);
    expect(fetches.get('/api/v1/media/shot-b')).toBe(1);
    // Возврат к первой картинке оригинал не докачивает: он уже раскодирован и лежит в кеше.
    await page.keyboard.press('ArrowLeft');
    await expect(dialog.locator('.counter')).toHaveText('1 из 3');
    await settled(page);
    expect(fetches.get('/api/v1/media/shot-a')).toBe(1);
    await expect(dialog.locator('.slide[data-current] img.full')).toHaveAttribute('src', '/api/v1/media/shot-a');
    // Показ и оригинал отдаются с кешем браузера.
    expect(cached.get('/api/v1/media/shot-a')).toBe('private, max-age=86400, immutable');
    expect(cached.get('/api/v1/media/shot-a/preview')).toBe('private, max-age=86400, immutable');
  });

  test('shared_files_use_previews_and_fetch_full_once', async ({ page }) => {
    const fetches = new Map<string, number>();
    page.on('request', (request) => {
      const path = new URL(request.url()).pathname;
      if (path.startsWith('/api/v1/attachments/')) fetches.set(path, (fetches.get(path) ?? 0) + 1);
    });
    await openChat(page, 'shared');
    const thumbs = page.locator('.files a.thumb img');
    await expect(thumbs.first()).toBeVisible();
    for (const src of await thumbs.evaluateAll((nodes) => nodes.map((node) => (node as HTMLImageElement).currentSrc))) {
      expect(new URL(src).pathname.endsWith('/preview')).toBe(true);
    }
    await thumb(page, 'shared', 1).click();
    const dialog = viewer(page);
    await expect(dialog.locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
    await settled(page);
    expect(fetches.get('/api/v1/attachments/shot-b/preview')).toBeGreaterThanOrEqual(1);
    expect(fetches.get('/api/v1/attachments/shot-b/content')).toBe(1);
  });
});
