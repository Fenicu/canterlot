import type { Page } from '@playwright/test';
import { test, expect } from './fixtures';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';

test.beforeEach(({ browserName }, info) => {
  test.skip(info.title.includes('@chromium') && browserName !== 'chromium', 'Проверка Chromium');
});

async function boot(page: Page) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  await page.goto(`/c/${ids.pinned}`);
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toBeVisible();
  return { hub, api, ids };
}

test('fresh_load_leaves_no_service_worker_and_no_shell_caches', async ({ page }) => {
  await boot(page);
  await expect.poll(() => page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length)).toBe(0);
  expect(await page.evaluate(() => caches.keys())).toEqual([]);
  expect((await page.request.get('/service-worker.js')).status()).toBe(404);
});

test('manifest_and_icons_are_served_with_credentials_and_valid', async ({ page }) => {
  await boot(page);
  await expect(page.locator('link[rel="manifest"]')).toHaveAttribute('crossorigin', 'use-credentials');
  const result = await page.evaluate(async () => {
    const response = await fetch('/manifest.webmanifest', { credentials: 'include' }); const manifest = await response.json();
    const icons = await Promise.all(manifest.icons.map(async (icon: { src: string }) => {
      const r = await fetch(icon.src, { credentials: 'include' }); const bitmap = await createImageBitmap(await r.blob()); return [bitmap.width, bitmap.height];
    }));
    return { manifest, icons };
  });
  expect(result.manifest).toMatchObject({ name: 'Canterlot', scope: '/' });
  expect(result.manifest).not.toHaveProperty('share_target');
  expect(result.icons).toEqual([[192, 192], [512, 512], [512, 512]]);
  await expect(page.locator('meta[name="theme-color"]')).not.toHaveAttribute('content', '');
});

test('install_menu_prompts_once_and_hides_after_install_in_both_themes', async ({ page }) => {
  await page.addInitScript(() => {
    window.addEventListener('beforeinstallprompt', event => {
      if (event.isTrusted) { event.preventDefault(); event.stopImmediatePropagation(); }
    }, true);
  });
  await boot(page);
  await page.goto('/sessions');
  await expect(page.getByRole('button', { name: 'Ещё', exact: true })).toBeVisible();
  await page.evaluate(() => {
    window.dispatchEvent(Object.assign(new Event('beforeinstallprompt', { cancelable: true }), {
      prompt: async () => { document.documentElement.dataset.installPrompt = 'called'; },
      userChoice: Promise.resolve({ outcome: 'accepted' })
    }));
  });
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Установить', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('html')).toHaveAttribute('data-install-prompt', 'called');
  await page.getByRole('button', { name: 'Сменить тему', exact: true }).click();
  await page.evaluate(() => window.dispatchEvent(new Event('appinstalled')));
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await expect(page.getByRole('menuitem', { name: 'Установить', exact: true })).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test('share_post_without_an_active_worker_is_redirected_to_the_explanation_by_the_edge', async ({ request }) => {
  const response = await request.post('/_share', { multipart: { text: 'Текст' }, maxRedirects: 0 });
  expect(response.status()).toBe(303); expect(response.headers().location).toBe('/share?error=sw_inactive');
});

test('csp_violations_are_zero_on_every_route', async ({ page }) => {
  const { ids } = await boot(page);
  for (const route of ['/sessions', '/', '/new', '/machines', '/limits', `/c/${ids.pinned}`, '/share?id=missing', '/__test/primitives']) {
    const response = await page.goto(route); expect(response?.headers()['content-security-policy']).toContain("frame-ancestors 'none'");
    await expect(page.locator('[data-layout]')).toBeVisible();
    expect(await page.evaluate(() => (window as unknown as { __csp?: string[] }).__csp ?? [])).toEqual([]);
  }
});

test('reduced_motion_disables_sheet_and_caret_animation', async ({ page }) => {
  const { hub, api, ids } = await boot(page);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const blockId = api.id();
  hub.append(`conversation:${ids.pinned}`, { kind: 'block_started', conversation_id: ids.pinned, payload: { block_id: blockId, rev: 0, kind: 'text', input_command_id: null, parent_block_id: null, agent: 'агент', model: 'opus', tool_name: null, summary: null, approved_by: null } });
  hub.append(`conversation:${ids.pinned}`, { kind: 'block_delta', conversation_id: ids.pinned, payload: { block_id: blockId, base_rev: 0, rev: 1, append: 'Продолжаю ответ' } });
  hub.flush();
  await expect(page.locator('.caret')).toBeVisible();
  await expect(page.locator('.caret')).toHaveCSS('animation-name', 'none');
  await page.getByRole('button', { name: 'Отправить позже', exact: true }).count().then(async count => {
    if (count) await page.getByRole('button', { name: 'Отправить позже', exact: true }).click();
    else await page.getByRole('textbox', { name: 'Сообщение', exact: true }).press('Control+Shift+Enter');
  });
  await expect(page.getByRole('dialog')).toBeVisible();
  expect(await page.evaluate(() => document.getAnimations().filter(a => a.playState === 'running').length)).toBe(0);
  await page.getByRole('dialog').getByRole('button', { name: 'Закрыть', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('reload_while_an_upload_is_in_flight_resumes_it', async ({ page }) => {
  await boot(page);
  let first = true; let release!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/v1/attachments/*/content', async route => {
    if (route.request().method() === 'PATCH' && first) { first = false; await blocked; await route.abort().catch(() => {}); return; }
    await route.fallback();
  });
  await page.locator('input[type="file"]').setInputFiles({ name: 'resume.txt', mimeType: 'text/plain', buffer: Buffer.alloc(10000, 65) });
  await expect.poll(() => first).toBe(false);
  await page.reload(); release();
  await expect(page.getByRole('progressbar', { name: 'resume.txt' })).toHaveAttribute('aria-valuenow', '10000');
  await expect.poll(() => page.evaluate(async () => {
    const db = await new Promise<IDBDatabase>((resolve, reject) => { const r = indexedDB.open('canterlot', 1); r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error); });
    const rows = await new Promise<{ phase: string }[]>(resolve => { const r = db.transaction('uploads').objectStore('uploads').getAll(); r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error); }); db.close(); return rows.map(r => r.phase);
  })).toEqual([]);
});
