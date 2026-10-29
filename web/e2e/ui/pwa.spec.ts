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
  await page.evaluate(async () => { await navigator.serviceWorker.ready; });
  await page.waitForFunction(() => !!navigator.serviceWorker.controller);
  return { hub, api, ids };
}

test('offline_shell_opens_after_the_first_visit_and_labels_staleness @chromium', async ({ page, context }) => {
  const { ids } = await boot(page);
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).fill('Черновик без сети');
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).blur();
  await context.setOffline(true);
  await page.unrouteAll({ behavior: 'wait' });
  await page.reload();
  await expect(page.locator('[data-offline]')).toContainText(/Нет связи · данные от \d{2}:\d{2}/);
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Черновик без сети');
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).fill('Изменён без сети');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Изменён без сети');
  await page.goto('/new'); await expect(page.locator('[data-offline]')).toBeVisible();
  await page.goto('/machines'); await expect(page.locator('[data-offline]')).toBeVisible();
  await page.goto(`/c/${ids.pinned}`); await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Изменён без сети');
});

test('deep_link_opens_a_chat_from_the_cold_offline_shell', async ({ page, context }) => {
  const { ids } = await boot(page);
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).fill('После закрытия');
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).blur();
  await page.close(); await context.setOffline(true);
  const cold = await context.newPage(); await cold.goto(`/c/${ids.pinned}`);
  await expect(cold.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('После закрытия');
  await expect(cold.locator('[data-offline]')).toBeVisible();
});

test('service_worker_does_not_cache_api_responses', async ({ page }) => {
  await boot(page);
  await page.evaluate(async () => { await fetch('/api/v1/me'); await fetch('/healthz'); });
  const urls = await page.evaluate(async () => (await Promise.all((await caches.keys()).map(async key => (await (await caches.open(key)).keys()).map(req => req.url)))).flat());
  expect(urls.some(url => url.endsWith('/200.html'))).toBe(true);
  expect(urls.some(url => /\/api(?:\/|$)|\/healthz/.test(url))).toBe(false);
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
  expect(result.manifest).toMatchObject({ name: 'canterlot', scope: '/', share_target: { action: '/_share' } });
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

test('update_flow_does_not_reload_while_typing_and_activates_after_flush @chromium', async ({ page, context, baseURL }) => {
  const { ids } = await boot(page);
  await page.getByRole('textbox', { name: 'Сообщение', exact: true }).fill('Не потерять при обновлении');
  await page.evaluate(() => { (window as unknown as { reloadSentinel: number }).reloadSentinel = 1; });
  await context.addCookies([{ name: 'pwa-build', value: 'next', url: baseURL! }]);
  await page.evaluate(async () => { const reg = await navigator.serviceWorker.ready; await reg.update(); });
  await expect(page.locator('[data-update]')).toContainText('Доступна новая версия');
  await page.waitForFunction(async () => (await navigator.serviceWorker.ready).waiting?.state === 'installed');
  await expect.poll(() => page.evaluate(() => (window as unknown as { reloadSentinel: number }).reloadSentinel)).toBe(1);
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Не потерять при обновлении');
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await page.waitForFunction(() => !(window as unknown as { reloadSentinel?: number }).reloadSentinel);
  await expect(page.getByRole('textbox', { name: 'Сообщение', exact: true })).toHaveValue('Не потерять при обновлении');
  await expect(page).toHaveURL(new RegExp(`/c/${ids.pinned}`));
});

test('share_post_is_stashed_and_redirected_by_the_worker @chromium', async ({ page }) => {
  await boot(page);
  const result = await page.evaluate(async () => {
    const form = new FormData(); form.set('title', 'Название'); form.set('text', 'Текст'); form.set('url', 'https://example.org/'); form.append('files', new File(['payload'], 'sample.txt'));
    const response = await fetch('/_share', { method: 'POST', body: form });
    const db = await new Promise<IDBDatabase>((resolve, reject) => { const r = indexedDB.open('canterlot', 1); r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error); });
    const values = await new Promise<unknown[]>((resolve, reject) => { const r = db.transaction('share_inbox').objectStore('share_inbox').getAll(); r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error); });
    db.close(); return { url: response.url, values: values.map(value => { const v = value as { files: File[]; title: string; text: string; url: string }; return { ...v, files: v.files.map(f => ({ name: f.name, size: f.size })) }; }) };
  });
  expect(result.url).toMatch(/\/share\?id=/);
  expect(result.values).toEqual([expect.objectContaining({ title: 'Название', text: 'Текст', url: 'https://example.org/', files: [{ name: 'sample.txt', size: 7 }] })]);
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
  hub.append(`conversation:${ids.pinned}`, { kind: 'block_started', conversation_id: ids.pinned, payload: { block_id: blockId, rev: 0, kind: 'text', input_command_id: null, parent_block_id: null, agent: 'агент', model: 'opus', tool_name: null, summary: null } });
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
    const db = await new Promise<IDBDatabase>(resolve => { const r = indexedDB.open('canterlot', 1); r.onsuccess = () => resolve(r.result); });
    const rows = await new Promise<{ phase: string }[]>(resolve => { const r = db.transaction('uploads').objectStore('uploads').getAll(); r.onsuccess = () => resolve(r.result); }); db.close(); return rows.map(r => r.phase);
  })).toEqual([]);
});
