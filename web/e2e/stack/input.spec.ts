import { test, expect, type Page } from '@playwright/test';
import { getStack } from './global-setup';
import { synthetic } from './synthetic';
import { composerOf, createHubClient, createSessionUI, sendButtonOf, sendChaos, sendScript } from './hub';

const tag = { tag: '@stack' };
const chromiumOnly = { tag: ['@stack', '@chromium'] };

async function center(locator: ReturnType<Page['locator']>): Promise<{ x: number; y: number }> {
  const box = (await locator.boundingBox())!;
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

test.describe('stack: input and queue', () => {
  test('mobile_input_flow_on_the_phone_viewport', chromiumOnly, async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'phone', 'Касание и грубый указатель есть только в телефонном профиле Chromium');
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const composer = composerOf(page);
    const send = sendButtonOf(page);
    const cdp = await page.context().newCDPSession(page);
    expect(await page.evaluate(() => matchMedia('(pointer: coarse)').matches), 'профиль телефона: грубый указатель').toBe(true);

    // 1. Enter на касании вставляет перенос строки и ничего не отправляет.
    await page.touchscreen.tap(...Object.values(await center(composer)) as [number, number]);
    await expect(composer).toBeFocused();
    await page.keyboard.type('Строка 1');
    await page.keyboard.press('Enter');
    await page.keyboard.type('Строка 2');
    await expect(composer).toHaveValue('Строка 1\nСтрока 2');

    // 2. Составной ввод (IME): Enter внутри композиции не отправляет.
    await composer.fill('');
    await cdp.send('Input.imeSetComposition', { text: 'приве', selectionStart: 5, selectionEnd: 5 });
    await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
    await cdp.send('Input.insertText', { text: 'привет' });
    await expect(composer).toHaveValue('привет');
    expect(await hub.inputs(id), 'композиция не отправила ничего').toHaveLength(0);

    // 3. Вставка 300 КиБ не отправляет, а кнопка блокируется с причиной текстом.
    await composer.fill('');
    await composer.focus();
    await cdp.send('Input.insertText', { text: 'Ж'.repeat(300 * 1024 / 2) });
    await expect(send).toHaveAttribute('aria-disabled', 'true');
    await expect(page.locator('[data-composer] .reason')).toContainText(/превыш|слишком|лимит|КиБ/i);
    expect(await hub.inputs(id), 'вставка не отправила ничего').toHaveLength(0);

    // 4. Возвращаем осмысленный текст.
    await composer.fill('Строка 1\nСтрока 2');
    await expect(send).toHaveAttribute('aria-disabled', 'false');

    // 5. Долгое нажатие на «Отправить» открывает окно отправки позже и ничего не шлёт.
    const at = await center(send);
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [at] });
    await page.waitForTimeout(700);
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    await expect(page.getByRole('dialog').first()).toBeVisible();
    expect(await hub.inputs(id), 'долгое нажатие не отправило ничего').toHaveLength(0);
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(composer).toHaveValue('Строка 1\nСтрока 2');

    // 6. Двойное касание по «Отправить» создаёт ровно одну операцию.
    await page.touchscreen.tap(at.x, at.y);
    await page.touchscreen.tap(at.x, at.y);
    await expect.poll(async () => (await hub.inputs(id)).length, { timeout: 15_000 }).toBe(1);
    await page.waitForTimeout(1500);
    const inputs = await hub.inputs(id);
    expect(inputs).toHaveLength(1);
    expect(inputs[0]!.content[0]!.text).toBe('Строка 1\nСтрока 2');
  });

  test('response_lost_after_commit_creates_one_input', tag, async ({ page, browserName }) => {
    test.setTimeout(60_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);

    const posts: string[] = [];
    const lost: string[] = [];
    page.on('request', (request) => {
      if (request.method() === 'POST' && /\/conversations\/[^/]+\/inputs$/.test(new URL(request.url()).pathname)) posts.push(request.headers()['idempotency-key'] ?? '');
    });
    page.on('requestfailed', (request) => {
      if (request.method() === 'POST' && request.url().endsWith('/inputs')) lost.push(request.failure()?.errorText ?? '');
    });
    // События канала молчат, пока ответ потерян: операция не может закрыться по событию и обязана повторить запрос сама.
    await sendChaos(stack.control, { action: 'blackhole', on: true, ws: true });
    await sendChaos(stack.control, { action: 'drop_response_once', pathRegex: '/conversations/[^/]+/inputs$', method: 'POST' });
    await composerOf(page).fill(synthetic.script([synthetic.delta('ответ на потерянный ответ')]));
    await sendButtonOf(page).click();

    try {
      // Firefox сам повторяет POST на оборванном соединении: страница не видит ни отказа, ни второго запроса.
      if (browserName === 'firefox') {
        await expect.poll(async () => (await hub.inputs(id)).length, { timeout: 40_000 }).toBe(1);
      } else {
        await expect.poll(() => posts.length, { timeout: 40_000, message: 'клиент повторил запрос после потери ответа' }).toBeGreaterThanOrEqual(2);
        expect(lost.length, 'кромка потеряла ответ на коммит').toBe(1);
      }
      expect(new Set(posts).size, 'повтор идёт с тем же ключом идемпотентности').toBe(1);
    } finally {
      // Канал, оставленный молчать, обрушил бы все следующие проверки стенда.
      await sendChaos(stack.control, { action: 'blackhole', on: false, ws: true });
    }

    await expect(page.locator('article.card [data-block]')).toContainText('ответ на потерянный ответ', { timeout: 40_000 });
    expect(await hub.inputs(id)).toHaveLength(1);
    await expect(page.locator('article.bubble')).toHaveCount(1);
    await expect(page.locator('.strip')).not.toContainText('Сохраняется');
    await expect(page.locator('.strip')).not.toContainText('Отправка не подтверждена');
  });

  test('offline_draft_then_reconnect_sends_nothing_by_itself', tag, async ({ page, context }) => {
    test.setTimeout(60_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const composer = composerOf(page);

    await context.setOffline(true);
    await sendChaos(stack.control, { action: 'cut' });
    await composer.fill('черновик без связи');
    await sendButtonOf(page).click();
    await expect(page.locator('[data-composer] .reason')).toContainText('Нет связи');
    await expect(composer).toHaveValue('черновик без связи');

    await context.setOffline(false);
    await page.waitForTimeout(4000);
    expect(await hub.inputs(id), 'после возвращения связи ничего не отправлено само').toHaveLength(0);
    await expect(composer).toHaveValue('черновик без связи');
    await expect(page.locator('article.bubble')).toHaveCount(0);

    // Только явное нажатие отправляет черновик.
    await sendButtonOf(page).click();
    await expect.poll(async () => (await hub.inputs(id)).length, { timeout: 15_000 }).toBe(1);
    expect((await hub.inputs(id))[0]!.content[0]!.text).toBe('черновик без связи');
  });

  test('queue_scheduled_cancel_interrupt_flow', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const composer = composerOf(page);

    await sendScript(page, synthetic.script([synthetic.delta('A-идёт'), synthetic.hang()]));
    await expect(page.locator('article.card [data-block]')).toContainText('A-идёт', { timeout: 20_000 });

    // Два сообщения встают в очередь за зависшим ходом.
    await sendScript(page, synthetic.script([synthetic.delta('Q1-ответ')]));
    await sendScript(page, synthetic.script([synthetic.delta('Q2-ответ')]));
    const strip = page.locator('.strip');
    await expect(strip.getByRole('button', { name: 'Отменить' })).toHaveCount(2, { timeout: 15_000 });

    // Отложенное сообщение через 5 с появляется в полосе и ждёт своей очереди.
    const runAt = new Date(Date.now() + 5000).toISOString();
    await hub.json(`/api/v1/conversations/${id}/scheduled`, { method: 'POST', body: JSON.stringify({ run_at: runAt, content: [{ type: 'text', text: synthetic.script([synthetic.delta('S-ответ')]) }] }) });
    await expect(strip.locator('.item:not([data-state])').getByRole('button', { name: 'Изменить' })).toHaveCount(1, { timeout: 15_000 });

    // Отменяем второе сообщение очереди: остаётся одно «Отменить» очереди.
    await strip.locator('[data-state]').filter({ hasText: 'Q2-ответ' }).getByRole('button', { name: 'Отменить' }).click();
    await expect.poll(async () => (await hub.inputLog(id)).find((item) => item.text.includes('Q2-ответ'))?.state, { timeout: 15_000 }).toBe('cancelled');

    // Отложенное время наступило, но ход A ещё занят: сообщение уходит в очередь.
    await expect.poll(async () => (await hub.inputLog(id)).some((item) => item.text.includes('S-ответ')), { timeout: 30_000 }).toBe(true);
    expect(await page.locator('article.card [data-block]').allTextContents()).not.toContain('Q1-ответ');

    // Прерываем A: очередь встаёт на паузу, «Продолжить очередь» запускает её по порядку Q1, S.
    await page.getByRole('button', { name: 'Прервать', exact: true }).click();
    const proceed = page.getByRole('button', { name: 'Продолжить очередь', exact: true });
    await expect(proceed).toBeVisible({ timeout: 30_000 });
    await proceed.click();
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'Q1-ответ' })).toBeVisible({ timeout: 40_000 });
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'S-ответ' })).toBeVisible({ timeout: 40_000 });
    const text = (await page.locator('article.card [data-block]').allTextContents()).join('|');
    expect(text.indexOf('Q1-ответ')).toBeLessThan(text.indexOf('S-ответ'));
    expect(text).not.toContain('Q2-ответ');
    await hub.waitIdle(id);
    const states = Object.fromEntries((await hub.inputLog(id)).map((item) => [item.text.match(/(A-идёт|Q1-ответ|Q2-ответ|S-ответ)/)![1], item.state]));
    expect(states['Q2-ответ']).toBe('cancelled');
    expect(states['Q1-ответ']).toBe('accepted');
    expect(states['S-ответ']).toBe('accepted');
    void composer;
  });

  test('reload_keeps_a_queued_input_in_the_strip_and_delivers_its_bubble', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);

    await sendScript(page, synthetic.script([synthetic.delta('Z-идёт'), synthetic.hang()]));
    await expect(page.locator('article.card [data-block]')).toContainText('Z-идёт', { timeout: 20_000 });
    await sendScript(page, synthetic.script([synthetic.delta('Z-ответ')]));
    const strip = page.locator('.strip');
    await expect(strip.getByRole('button', { name: 'Отменить' })).toHaveCount(1, { timeout: 15_000 });

    await page.reload();
    await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
    await expect(strip.locator('[data-state="queued"]')).toContainText('Z-ответ', { timeout: 15_000 });
    await expect(strip.getByRole('button', { name: 'Отменить' })).toHaveCount(1);

    await page.getByRole('button', { name: 'Прервать', exact: true }).click();
    await page.getByRole('button', { name: 'Продолжить очередь', exact: true }).click({ timeout: 30_000 });
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'Z-ответ' })).toBeVisible({ timeout: 40_000 });
    await expect(page.locator('article.bubble').filter({ hasText: 'Z-ответ' })).toBeVisible();
    await expect(strip.getByRole('button', { name: 'Отменить' })).toHaveCount(0);
    await hub.waitIdle(id);
    expect(await hub.inputs(id)).toHaveLength(2);
  });

  test('cancelled_and_fired_scheduled_messages_leave_the_strip', tag, async ({ page }) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const strip = page.locator('.strip');
    const schedule = (run_at: string, text: string) =>
      hub.json(`/api/v1/conversations/${id}/scheduled`, { method: 'POST', body: JSON.stringify({ run_at, content: [{ type: 'text', text }] }) });

    await schedule(new Date(Date.now() + 3_600_000).toISOString(), 'Z-отменить');
    await expect(strip).toContainText('Z-отменить', { timeout: 15_000 });
    await strip.getByRole('button', { name: 'Отменить' }).click();
    await expect(strip).not.toContainText('Z-отменить', { timeout: 15_000 });

    await schedule(new Date(Date.now() + 3000).toISOString(), synthetic.script([synthetic.delta('Z-сработало')]));
    await expect(strip.locator('.item:not([data-state])').getByRole('button', { name: 'Изменить' })).toHaveCount(1, { timeout: 15_000 });
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'Z-сработало' })).toBeVisible({ timeout: 30_000 });
    await expect(strip.locator('.item:not([data-state])').getByRole('button', { name: 'Изменить' })).toHaveCount(0);

    await page.reload();
    await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'Z-сработало' })).toBeVisible({ timeout: 15_000 });
    await expect(strip.locator('.item:not([data-state])').getByRole('button', { name: 'Изменить' })).toHaveCount(0);
    await expect(strip).not.toContainText('Z-отменить');
  });

  test('interrupt_on_a_harness_that_exits_resumes_the_queue_end_to_end', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack, { agent: 'Синтетический с выходом при прерывании' });

    await sendScript(page, synthetic.script([synthetic.delta('висим'), synthetic.hang()]));
    await expect(page.locator('article.card [data-block]')).toContainText('висим', { timeout: 20_000 });
    await sendScript(page, synthetic.script([synthetic.delta('ответ из очереди после возобновления')]));
    await expect(page.locator('.strip').getByRole('button', { name: 'Отменить' })).toHaveCount(1, { timeout: 15_000 });

    await page.getByRole('button', { name: 'Прервать', exact: true }).click();
    const resume = page.getByRole('button', { name: 'Возобновить', exact: true });
    await expect(resume).toBeVisible({ timeout: 40_000 });
    await resume.click();

    await expect(page.locator('article.card [data-block]').filter({ hasText: 'ответ из очереди после возобновления' })).toBeVisible({ timeout: 60_000 });
    await hub.waitIdle(id);
    const states = (await hub.inputs(id)).map((item) => item.state);
    expect(states).toEqual(['accepted', 'accepted']);
  });
});
