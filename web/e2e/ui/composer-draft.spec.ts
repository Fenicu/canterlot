import { devices } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d2';

// Два контекста одного браузера: телефон и компьютер делят серверный черновик.
test.skip(({ browserName }) => browserName !== 'chromium', 'два контекста в chromium');

async function bootPair(
  browser: import('@playwright/test').Browser,
  baseURL: string,
) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const conversation = seedConversation(api, { title: 'Общий черновик' });
  seedHistory(api, conversation, 1);
  const phoneContext = await browser.newContext({
    ...devices['Pixel 7'],
    baseURL,
  });
  const desktopContext = await browser.newContext({
    baseURL,
    viewport: { width: 1440, height: 900 },
  });
  const phone = await phoneContext.newPage();
  const desktop = await desktopContext.newPage();
  await installChatRoutes(phone, hub, api, OWNER);
  await installChatRoutes(desktop, hub, api, OWNER);
  await phone.goto(`/c/${conversation}`);
  await desktop.goto(`/c/${conversation}`);
  await expect(
    phone.getByRole('textbox', { name: 'Сообщение' }),
  ).toBeVisible({ timeout: 15_000 });
  await expect(
    desktop.getByRole('textbox', { name: 'Сообщение' }),
  ).toBeVisible({ timeout: 15_000 });
  return {
    hub,
    api,
    conversation,
    phone,
    desktop,
    phoneContext,
    desktopContext,
    phoneComposer: phone.getByRole('textbox', { name: 'Сообщение' }),
    desktopComposer: desktop.getByRole('textbox', { name: 'Сообщение' }),
  };
}

test('shared_draft_text_and_attachments_follow_both_devices', async ({
  browser,
}, testInfo) => {
  test.setTimeout(120_000);
  const baseURL = testInfo.project.use.baseURL as string;
  const pair = await bootPair(browser, baseURL);

  await pair.phoneComposer.fill('черновик с телефона');
  await expect
    .poll(() => pair.api.draft(pair.conversation).text, { timeout: 10_000 })
    .toBe('черновик с телефона');
  await expect(pair.desktopComposer).toHaveValue('черновик с телефона');

  await pair.phone
    .locator('input[type="file"]')
    .setInputFiles({
      name: 'план.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('abc'),
    });
  await expect(
    pair.phone.locator('.tray').getByText('план.txt'),
  ).toBeVisible();
  await expect
    .poll(
      () => pair.api.draft(pair.conversation).attachment_ids?.length ?? 0,
      { timeout: 10_000 },
    )
    .toBe(1);
  await expect(
    pair.desktop.locator('.tray').getByText('план.txt'),
  ).toBeVisible({ timeout: 10_000 });

  await pair.desktop.getByRole('button', { name: 'Убрать' }).click();
  await expect(
    pair.desktop.locator('.tray').getByText('план.txt'),
  ).toHaveCount(0);
  await expect
    .poll(
      () => pair.api.draft(pair.conversation).attachment_ids?.length ?? 0,
      { timeout: 10_000 },
    )
    .toBe(0);
  await expect(
    pair.phone.locator('.tray').getByText('план.txt'),
  ).toHaveCount(0, { timeout: 10_000 });

  await pair.desktopComposer.fill('');
  await expect(pair.phoneComposer).toHaveValue('', { timeout: 10_000 });
  await expect
    .poll(() => pair.api.draft(pair.conversation).text, { timeout: 10_000 })
    .toBe('');
  expect(pair.api.draft(pair.conversation).attachment_ids).toEqual([]);

  await pair.phoneComposer.fill('переживёт перезагрузку');
  await expect
    .poll(() => pair.api.draft(pair.conversation).text, { timeout: 10_000 })
    .toBe('переживёт перезагрузку');
  await pair.phone.reload();
  await expect(
    pair.phone.getByRole('textbox', { name: 'Сообщение' }),
  ).toHaveValue('переживёт перезагрузку', { timeout: 15_000 });

  await pair.phoneContext.close();
  await pair.desktopContext.close();
});

test('sending_from_one_device_clears_both_composers_and_carries_attachments', async ({
  browser,
}, testInfo) => {
  test.setTimeout(120_000);
  const baseURL = testInfo.project.use.baseURL as string;
  const pair = await bootPair(browser, baseURL);
  const sent: Array<Record<string, unknown>> = [];
  pair.phone.on('request', (request) => {
    if (
      request.method() === 'POST' &&
      new URL(request.url()).pathname.endsWith('/inputs')
    ) {
      sent.push(request.postDataJSON() as Record<string, unknown>);
    }
  });

  await pair.phoneComposer.fill('отправлю с файлом');
  await pair.phone
    .locator('input[type="file"]')
    .setInputFiles({
      name: 'отчёт.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('данные'),
    });
  await expect(
    pair.phone.locator('.tray').getByText('отчёт.txt'),
  ).toBeVisible();
  await expect
    .poll(
      () => pair.api.draft(pair.conversation).attachment_ids?.length ?? 0,
      { timeout: 10_000 },
    )
    .toBe(1);
  await expect(
    pair.desktop.locator('.tray').getByText('отчёт.txt'),
  ).toBeVisible({ timeout: 10_000 });

  await pair.phone.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect.poll(() => sent.length, { timeout: 10_000 }).toBe(1);
  expect(sent[0]?.attachment_ids).toHaveLength(1);
  await expect(pair.phoneComposer).toHaveValue('');
  await expect(pair.desktopComposer).toHaveValue('', { timeout: 10_000 });
  await expect(
    pair.desktop.locator('.tray').getByText('отчёт.txt'),
  ).toHaveCount(0, { timeout: 10_000 });
  expect(pair.api.draft(pair.conversation).text).toBe('');
  expect(pair.api.draft(pair.conversation).attachment_ids).toEqual([]);

  await pair.phoneContext.close();
  await pair.desktopContext.close();
});
