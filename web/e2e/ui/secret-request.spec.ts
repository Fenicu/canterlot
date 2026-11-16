import type { Page, Request } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const PROBE = 'canterlot-secret-PROBE-123';

async function bootChat(page: Page): Promise<{ hub: FakeHub; api: FakeHubApi; id: string }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);

  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [
      {
        id: 'claude',
        title: 'Claude Code',
        provider: 'claude',
        models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
        permission_modes: ['owner', 'ask', 'bypass', 'read_only']
      }
    ]
  });

  const id = seedConversation(api, {
    title: 'Сессия секретов',
    model: 'opus-5.5',
    effort: 'medium',
    permission_mode: 'owner',
    activity: { state: 'idle', evidence: {}, since: new Date().toISOString() }
  });
  const detail = api.details.get(id);
  if (detail) {
    detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
    detail.machine = { id: 'workstation', name: 'workstation' };
    detail.model = 'opus-5.5';
    detail.effort = 'medium';
    detail.permission_mode = 'owner';
  }

  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

  return { hub, api, id };
}

function openRequest(api: FakeHubApi, conversationId: string, patch: Record<string, unknown> = {}) {
  return api.openSecretRequest(conversationId, {
    name: 'Токен деплоя',
    purpose: 'Загрузить сборку на сервер',
    ...patch
  });
}

const card = (page: Page) => page.locator('[data-secret-request]');

test.describe('E2E Secret request card', () => {
  test('secret_request_covers_every_state', async ({ page }) => {
    const { api, id } = await bootChat(page);
    const view = openRequest(api, id);

    await expect(card(page)).toBeVisible();
    await expect(card(page).getByText('Агент просит секрет')).toBeVisible();
    await expect(card(page).getByText('Токен деплоя')).toBeVisible();
    await expect(card(page).getByText('Загрузить сборку на сервер')).toBeVisible();

    const field = card(page).locator('input[type="password"]');
    await expect(field).toBeVisible();
    await expect(field).toHaveAttribute('spellcheck', 'false');
    await expect(field).toHaveAttribute('autocapitalize', 'off');
    await expect(field).toHaveAttribute('autocorrect', 'off');
    await expect(field).toHaveAttribute('autocomplete', 'off');
    await expect(field).toHaveAttribute('data-1p-ignore', '');
    await expect(field).toHaveAttribute('data-lpignore', 'true');
    await expect(field).toHaveAttribute('data-bwignore', 'true');

    const states: Array<[string, string | RegExp]> = [
      ['sending', 'Передаю на машину…'],
      ['delivered', /^Передан · файл до \d{2}:\d{2}$/],
      ['declined', 'Отказано'],
      ['revoking', 'Отзываю…'],
      ['revoked', 'Отозван'],
      ['expired', 'Истёк'],
      ['lost', 'Значение не дошло до машины, введите снова'],
      ['cancelled', 'Отменён']
    ];
    for (const [state, text] of states) {
      api.setSecretRequest(view.id, {
        state,
        ...(state === 'delivered' ? { expires_at: '2026-11-16T11:30:00.000Z', path: '/tmp/canterlot-secret' } : {})
      });
      await expect(card(page).getByText(text)).toBeVisible();
      if (state === 'lost') await expect(card(page).locator('input[type="password"]')).toBeVisible();
    }
  });

  test('secret_request_decline', async ({ page }) => {
    const { api, id } = await bootChat(page);
    const view = openRequest(api, id);

    await card(page).getByRole('button', { name: 'Отказать' }).click();
    await expect(card(page).getByText('Отказано')).toBeVisible();
    expect(api.secretRequestAnswerBodies.at(-1)).toEqual({ decline: true });
    expect(api.secretRequest(view.id)?.state).toBe('declined');
  });

  test('secret_request_machine_offline_shows_an_inline_error_and_an_empty_field', async ({ page }) => {
    const { api, id } = await bootChat(page);
    openRequest(api, id);
    api.secretMachineOffline = true;

    const field = card(page).locator('input[type="password"]');
    await field.fill('s3cret-token');
    await card(page).getByRole('button', { name: 'Передать' }).click();

    await expect(card(page).getByText('Машина не на связи, попробуйте позже')).toBeVisible();
    await expect(field).toHaveValue('');
    expect(api.secretRequestAnswerBodies).toHaveLength(0);

    api.secretMachineOffline = false;
    await field.fill('s3cret-token');
    await card(page).getByRole('button', { name: 'Передать' }).click();
    await expect(card(page).getByText('Передаю на машину…')).toBeVisible();
    expect(api.secretRequestAnswerBodies.at(-1)).toEqual({ value: 's3cret-token' });
  });

  test('secret_request_lost_then_entered_again', async ({ page }) => {
    const { api, id } = await bootChat(page);
    const view = openRequest(api, id);

    api.setSecretRequest(view.id, { state: 'lost' });
    await expect(card(page).getByText('Значение не дошло до машины, введите снова')).toBeVisible();

    const field = card(page).locator('input[type="password"]');
    await field.fill('second-value');
    await card(page).getByRole('button', { name: 'Передать' }).click();
    await expect(card(page).getByText('Передаю на машину…')).toBeVisible();
    expect(api.secretRequestAnswerBodies.at(-1)).toEqual({ value: 'second-value' });

    api.setSecretRequest(view.id, { state: 'delivered', expires_at: '2026-11-16T11:30:00.000Z', path: '/tmp/canterlot-secret' });
    await expect(card(page).getByText(/^Передан · файл до /)).toBeVisible();
  });

  test('secret_request_multiline_keeps_pem_bytes', async ({ page }) => {
    const { api, id } = await bootChat(page);
    openRequest(api, id);
    const pem = '-----BEGIN PRIVATE KEY-----\nAAAA\nBBBB\n-----END PRIVATE KEY-----\n';

    await card(page).getByRole('button', { name: 'Несколько строк' }).click();
    const area = card(page).locator('textarea[data-secret-input]');
    await expect(area).toBeVisible();
    for (const [name, value] of [
      ['autocomplete', 'off'],
      ['autocapitalize', 'off'],
      ['autocorrect', 'off'],
      ['spellcheck', 'false'],
      ['data-1p-ignore', ''],
      ['data-lpignore', 'true'],
      ['data-bwignore', 'true']
    ] as const) {
      await expect(area).toHaveAttribute(name, value);
    }

    await area.fill(pem);
    await card(page).getByRole('button', { name: 'Передать' }).click();
    await expect(card(page).getByText('Передаю на машину…')).toBeVisible();
    expect(api.secretRequestAnswerBodies.at(-1)).toEqual({ value: pem });
  });

  test('secret_request_revoke', async ({ page }) => {
    const { api, id } = await bootChat(page);
    const view = openRequest(api, id);
    api.setSecretRequest(view.id, { state: 'delivered', expires_at: '2026-11-16T11:30:00.000Z', path: '/tmp/canterlot-secret' });

    await card(page).getByRole('button', { name: 'Отозвать' }).click();
    await expect(card(page).getByText('Отзываю…')).toBeVisible();
    expect(api.secretRequestRevokes.at(-1)).toBe(view.id);

    api.setSecretRequest(view.id, { state: 'revoked' });
    await expect(card(page).getByText('Отозван')).toBeVisible();
  });

  test('secret_request_card_shows_in_the_feed_and_in_the_waiting_lane', async ({ page }) => {
    const { api, id } = await bootChat(page);
    const view = openRequest(api, id);

    await expect(page.locator('.request-cards-slot [data-secret-request]')).toBeVisible();

    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(`[data-waiting="${id}"]`)).toBeVisible();
    await expect(page.getByRole('button', { name: /^Ждут вас/ })).toBeVisible();

    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
    await page.locator('[data-secret-request]').getByRole('button', { name: 'Отказать' }).click();
    await expect(page.locator('.feed [data-secret-request]').getByText('Отказано')).toBeVisible();
    expect(api.secretRequest(view.id)?.state).toBe('declined');
  });

  test('secret_request_value_never_reaches_storage_or_another_request_body', async ({ page }) => {
    const { api, id } = await bootChat(page);
    openRequest(api, id);

    const bodies: { url: string; body: string }[] = [];
    page.on('request', (request: Request) => {
      const body = request.postData();
      if (body) bodies.push({ url: request.url(), body });
    });

    const field = card(page).locator('input[type="password"]');
    await field.fill(PROBE);
    await card(page).getByRole('button', { name: 'Передать' }).click();
    await expect(card(page).getByText('Передаю на машину…')).toBeVisible();
    await expect(field).toHaveValue('');

    const answered = bodies.filter((item) => item.body.includes(PROBE));
    expect(answered).toHaveLength(1);
    expect(answered[0]!.url).toContain('/answer');
    expect(JSON.parse(answered[0]!.body)).toEqual({ value: PROBE });

    const stored = await page.evaluate(async (probe) => {
      const found: string[] = [];
      for (const [label, storage] of [
        ['localStorage', localStorage],
        ['sessionStorage', sessionStorage]
      ] as const) {
        for (let index = 0; index < storage.length; index++) {
          const key = storage.key(index) ?? '';
          if (key.includes(probe) || (storage.getItem(key) ?? '').includes(probe)) found.push(`${label}:${key}`);
        }
      }

      const serialize = async (value: unknown): Promise<string> => {
        if (value instanceof Blob) return await value.text();
        try {
          return JSON.stringify(value);
        } catch {
          return String(value);
        }
      };
      const databases = (await indexedDB.databases?.()) ?? [];
      for (const info of databases) {
        if (!info.name) continue;
        const db = await new Promise<IDBDatabase>((resolve, reject) => {
          const request = indexedDB.open(info.name!);
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
        });
        try {
          for (const storeName of db.objectStoreNames) {
            const values = await new Promise<unknown[]>((resolve, reject) => {
              const request = db.transaction(storeName).objectStore(storeName).getAll();
              request.onsuccess = () => resolve(request.result as unknown[]);
              request.onerror = () => reject(request.error);
            });
            for (const value of values) {
              const text = await serialize(value);
              if (text.includes(probe)) found.push(`indexedDB:${info.name}/${storeName}`);
            }
          }
        } finally {
          db.close();
        }
      }
      return found;
    }, PROBE);
    expect(stored).toEqual([]);
  });
});
