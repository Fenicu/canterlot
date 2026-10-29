import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect } from './fixtures';

export async function bootSecondary(page: Page, path = '/machines', logout: string | null = null) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  const fixture = await seedSecondaryScreens(page, api, ids, logout);
  await page.goto(path);
  await expect(page.locator('[data-layout]')).toBeVisible();
  return { hub, api, ids, ...fixture };
}

export async function postShare(page: Page) {
  await page.evaluate(async () => { await navigator.serviceWorker.ready; });
  await page.waitForFunction(() => !!navigator.serviceWorker.controller);
  return page.evaluate(async () => {
    const form = new FormData();
    form.set('title', 'Описание'); form.set('text', 'Сверь с кодом'); form.set('url', 'https://example.org/');
    form.append('files', new File(['payload'], 'notes.txt', { type: 'text/plain' }));
    return (await fetch('/_share', { method: 'POST', body: form })).url;
  });
}
