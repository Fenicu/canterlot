import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect } from './fixtures';

export async function bootSecondary(page: Page, path = '/machines', logout: string | null = null, patch?: (api: FakeHubApi) => void) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  patch?.(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  const fixture = await seedSecondaryScreens(page, api, ids, logout);
  await page.goto(path);
  await expect(page.locator('[data-layout]')).toBeVisible();
  return { hub, api, ids, ...fixture };
}

export interface SharePayload {
  title?: string;
  text?: string;
  url?: string;
  files?: { name: string; body: string; type?: string }[];
}

const SAMPLE: SharePayload = {
  title: 'Описание',
  text: 'Сверь с кодом',
  url: 'https://example.org/',
  files: [{ name: 'notes.txt', body: 'payload' }]
};

export async function postShare(page: Page, share: SharePayload = SAMPLE) {
  await page.evaluate(async () => { await navigator.serviceWorker.ready; });
  await page.waitForFunction(() => !!navigator.serviceWorker.controller);
  // Системное «Поделиться» открывает share target навигацией верхнего уровня: так и отправляем форму.
  await page.evaluate((data) => {
    const form = document.createElement('form');
    form.method = 'post'; form.enctype = 'multipart/form-data'; form.action = '/_share';
    for (const key of ['title', 'text', 'url'] as const) {
      const value = data[key];
      if (value === undefined) continue;
      const field = document.createElement('input');
      field.type = 'hidden'; field.name = key; field.value = value; form.append(field);
    }
    const picker = document.createElement('input');
    picker.type = 'file'; picker.name = 'files'; picker.multiple = true;
    const transfer = new DataTransfer();
    for (const file of data.files ?? []) transfer.items.add(new File([file.body], file.name, { type: file.type ?? 'text/plain' }));
    picker.files = transfer.files; form.append(picker);
    document.body.append(form); form.submit();
  }, share);
  await page.waitForURL(/\/share\?/);
  return page.url();
}
