import { test, expect, type Browser } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { getStack, SESSION_COOKIE, storageStateFor, type SignedIn } from './global-setup';
import { createHubClient } from './hub';

const tag = { tag: '@stack' };
const signedOut = { cookies: [], origins: [] };

async function contextFor(browser: Browser, signedIn: SignedIn) {
  return browser.newContext({ storageState: storageStateFor(getStack().baseUrl, signedIn) });
}

test.describe('собственный вход владельца', () => {
  test('qr_sign_in_between_two_browser_contexts', tag, async ({ browser, page }) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const fresh = await browser.newContext({ storageState: signedOut });
    const newcomer = await fresh.newPage();
    await newcomer.goto(`${stack.baseUrl}/sessions`);
    await expect(newcomer.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 20_000 });
    await newcomer.getByRole('button', { name: 'Войти по QR' }).click();
    const code = newcomer.getByRole('img', { name: 'Код входа в canterlot' });
    await expect(code).toBeVisible();
    const url = (await code.getAttribute('data-qr'))!;
    expect(url.startsWith(`${stack.baseUrl}/qr#`)).toBe(true);

    // Вошедшее устройство открывает ссылку кода, как после сканирования камерой.
    await page.goto(url);
    await expect(page).toHaveURL(`${stack.baseUrl}/qr`);
    const card = page.locator('[data-qr-approve]');
    await expect(card.getByText(/код показан \d+ с назад/)).toBeVisible({ timeout: 20_000 });
    await expect(newcomer.getByText('Код отсканирован, подтвердите на другом устройстве')).toBeVisible({ timeout: 30_000 });
    await card.getByRole('button', { name: 'Разрешить' }).click();
    await expect(card.getByText(/Вход разрешён/)).toBeVisible();

    await expect(newcomer.locator('[data-list-ready]')).toBeVisible({ timeout: 30_000 });
    const me = await newcomer.evaluate(async () => (await fetch('/api/v1/me')).json() as Promise<{ session_id: string }>);
    const sessions = await createHubClient(stack).json<{ sessions: { id: string; method: string; approved_by: string | null }[] }>('/api/v1/auth/sessions');
    const born = sessions.sessions.find((item) => item.id === me.session_id);
    expect(born?.method).toBe('qr');
    expect(born?.approved_by).not.toBeNull();
    await fresh.close();
  });

  test('passkey_registration_and_sign_in_with_a_virtual_authenticator', tag, async ({ browser, browserName }) => {
    test.skip(browserName !== 'chromium', 'виртуальный аутентификатор есть только в CDP Chromium');
    test.setTimeout(120_000);
    const stack = getStack();
    const context = await contextFor(browser, await stack.newSession());
    const page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    await cdp.send('WebAuthn.enable');
    await cdp.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true } });

    // Сеанс из ссылки восстановления свеж: ключ добавляется без повторного подтверждения.
    await page.goto(`${stack.baseUrl}/settings/sign-in`);
    const keys = page.locator('main [data-passkey]');
    await expect(page.locator('main[data-secondary-ready]')).toBeVisible({ timeout: 20_000 });
    const before = await keys.count();
    await page.getByRole('button', { name: 'Добавить ключ доступа' }).click();
    await expect(page.getByRole('status').filter({ hasText: 'Ключ доступа добавлен' })).toBeVisible({ timeout: 20_000 });
    await expect(keys).toHaveCount(before + 1);

    await page.goto(`${stack.baseUrl}/settings?c=security`);
    await page.locator('main').getByRole('button', { name: 'Выйти', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 20_000 });
    await page.getByRole('button', { name: 'Войти с ключом доступа' }).click();
    // После входа страница открывается заново там же: на экране настроек.
    await expect(page.locator('main[data-secondary-ready]')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByRole('heading', { name: 'Вход в canterlot' })).toHaveCount(0);
    const me = await page.evaluate(async () => (await fetch('/api/v1/me')).json() as Promise<{ session_id: string }>);
    const sessions = await createHubClient(stack).json<{ sessions: { id: string; method: string }[] }>('/api/v1/auth/sessions');
    expect(sessions.sessions.find((item) => item.id === me.session_id)?.method).toBe('passkey');

    // Сеанс по ключу свеж: ключ удаляется сразу, стенд остаётся без ключей.
    await page.goto(`${stack.baseUrl}/settings/sign-in`);
    await expect(keys).toHaveCount(before + 1, { timeout: 20_000 });
    await keys.last().getByRole('button', { name: /^Удалить ключ/ }).click();
    await page.getByRole('dialog', { name: 'Удалить ключ доступа?' }).getByRole('button', { name: 'Удалить' }).click();
    await expect(keys).toHaveCount(before);
    await context.close();
  });

  test('terminating_a_session_from_one_context_signs_the_other_out_and_wipes_it', tag, async ({ browser }) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const victimSession = await stack.newSession();
    const actorSession = await stack.newSession();
    const name = `Жертва-${randomUUID().slice(0, 6)}`;
    await createHubClient(stack).json(`/api/v1/auth/sessions/${victimSession.id}`, { method: 'PATCH', body: JSON.stringify({ name }) });

    const victimContext = await contextFor(browser, victimSession);
    const victim = await victimContext.newPage();
    await victim.goto(`${stack.baseUrl}/sessions`);
    await expect(victim.locator('[data-list-ready]')).toBeVisible({ timeout: 20_000 });
    await victim.evaluate(() => localStorage.setItem('canterlot.theme', 'light'));

    const actorContext = await contextFor(browser, actorSession);
    const actor = await actorContext.newPage();
    await actor.goto(`${stack.baseUrl}/settings/sign-in`);
    await actor.locator('main button[data-session]').filter({ hasText: name }).click();
    await actor.getByRole('dialog', { name }).getByRole('button', { name: 'Завершить сеанс' }).click();
    await actor.getByRole('dialog', { name: 'Завершить сеанс?' }).getByRole('button', { name: 'Завершить' }).click();
    await expect(actor.getByRole('status').filter({ hasText: 'Сеанс завершён' })).toBeVisible({ timeout: 20_000 });

    // Сокет закрывается с причиной revoked, страница входа заменяет приложение, данные стёрты.
    await expect(victim.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 20_000 });
    await expect(victim.getByText('Данные на этом устройстве удалены')).toBeVisible({ timeout: 10_000 });
    expect(await victim.evaluate(() => Object.keys(localStorage).filter((key) => key.startsWith('canterlot.')))).toEqual([]);

    // Следующий запрос с тем же cookie получает отзыв с причиной.
    const next = await fetch(`${stack.baseUrl}/api/v1/me`, { headers: { Cookie: `${SESSION_COOKIE}=${victimSession.session}` } });
    expect(next.status).toBe(401);
    const body = (await next.json()) as { error: { code: string; details?: { reason?: string } } };
    expect(body.error.code).toBe('session_revoked');
    expect(body.error.details?.reason).toBe('owner');

    await actor.goto(`${stack.baseUrl}/settings?c=security`);
    await actor.locator('main').getByRole('button', { name: 'Выйти', exact: true }).click();
    await expect(actor.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 20_000 });
    await victimContext.close();
    await actorContext.close();
  });
});
