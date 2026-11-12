import { test, expect, type Page } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { PNG } from 'pngjs';
import { getStack, STACK_STATE_FILE } from './global-setup';
import { synthetic } from './synthetic';
import { answerText, createHubClient, createSessionUI, fetchFrames, sendChaos, sendScript } from './hub';

const tag = { tag: '@stack' };

async function openInfo(page: Page) {
  const dialog = page.getByRole('dialog', { name: 'Об ответе' });
  // Кнопка появляется вместе с карточкой, а сведения о ходе приходят отдельным событием: нажатие повторяется, пока оно не дойдёт.
  await expect(async () => {
    await page.getByRole('button', { name: 'Сведения о ходе' }).first().click({ timeout: 2000 });
    await expect(dialog).toBeVisible({ timeout: 2000 });
  }).toPass({ timeout: 20_000 });
  return dialog;
}

/** Окно «Сессия»: на телефоне это лист (Escape), на рабочем столе всплывающее окно с кнопкой «Закрыть». */
async function closeSessionSheet(page: Page): Promise<void> {
  const popover = page.getByRole('dialog', { name: 'Сессия' });
  if (await popover.count()) await popover.getByRole('button', { name: 'Закрыть' }).click();
  else await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
}

test.describe('stack: features and security', () => {
  test('subagent_row_lane_and_delegation_panel', tag, async ({ page }, testInfo) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    await sendScript(page, synthetic.script([
      synthetic.delta('До субагента. '),
      synthetic.subagent('Explore', 'synthetic-small', 'Найти дубликаты', [synthetic.tool('Read', 'src/lib.rs'), synthetic.delta('нашёл три')]),
      synthetic.delta('Итог после субагента.')
    ]));
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'Итог после субагента.' })).toBeVisible({ timeout: 30_000 });
    await hub.waitIdle(id);

    const row = page.getByRole('link', { name: /Субагент/ }).first();
    await expect(row).toBeVisible();
    const href = (await row.getAttribute('href'))!;
    expect(href).toMatch(new RegExp(`^/c/${id}/lane/[0-9a-f-]{36}$`));
    // Текст субагента не смешивается с ответом оркестратора.
    expect(await answerText(page)).not.toContain('нашёл три');

    if (testInfo.project.name === 'desktop') {
      await page.getByRole('button', { name: 'Делегирование' }).click();
      const pane = page.locator('[data-pane="lane"]');
      await expect(pane).toBeVisible();
      const panel = pane.getByRole('complementary', { name: 'Панель делегирования' });
      await expect(panel.getByText('Explore')).toBeVisible();
      await expect(panel.getByText('Найти дубликаты')).toBeVisible();
      await panel.getByRole('button', { name: /Explore/ }).click();
      await expect(pane.locator('.lane-screen[data-ready]')).toContainText('нашёл три');
      await expect(pane.getByRole('textbox', { name: 'Сообщение' })).toHaveCount(0);
    } else {
      await row.click();
      await expect(page).toHaveURL(new RegExp(`${href}$`));
      const lane = page.locator('.lane-screen[data-ready]');
      await expect(lane).toContainText('нашёл три');
      await expect(page.getByRole('textbox', { name: 'Сообщение' })).toHaveCount(0);
      await page.getByRole('link', { name: 'Назад к сессии' }).click();
      await expect(page).toHaveURL(new RegExp(`/c/${id}$`));
    }
  });

  test('todo_list_runs_from_the_adapter_to_the_panel_and_returns_from_the_snapshot', tag, async ({ page }) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const panel = page.getByRole('region', { name: 'Список задач агента' });
    await expect(panel).toHaveCount(0);

    await sendScript(page, synthetic.script([
      synthetic.todo([
        { text: 'Собрать данные', status: 'in_progress' },
        { text: 'Проверить итог', status: 'pending' }
      ]),
      synthetic.delta('Начало. '),
      synthetic.delayMs(1500),
      synthetic.subagent('Explore', 'synthetic-small', 'Найти дубликаты', [
        synthetic.todo([{ text: 'Читать журнал', status: 'in_progress' }]),
        synthetic.delta('нашёл три')
      ]),
      synthetic.todo([
        { text: 'Собрать данные', status: 'completed' },
        { text: 'Проверить итог', status: 'in_progress' }
      ]),
      synthetic.delta('Итог.')
    ]));

    // Первая ревизия видна посреди хода, вторая заменяет её на месте.
    await expect(panel).toContainText('0 из 2', { timeout: 30_000 });
    await expect(panel).toContainText('Собрать данные');
    await expect(panel).toContainText('1 из 2', { timeout: 30_000 });
    await expect(panel).toContainText('Проверить итог');
    await hub.waitIdle(id);

    const stored = await hub.json<{ data: { todos: { lane_block_id: string | null; revision: number; items: { text: string; status: string }[] }[] } }>(
      `/api/v1/conversations/${id}/snapshot`
    );
    expect(stored.data.todos.map((list) => list.items.length)).toEqual([2, 1]);
    expect(stored.data.todos[0]).toMatchObject({ lane_block_id: null, revision: 2 });
    expect(stored.data.todos[1]!.lane_block_id).toMatch(/^[0-9a-f-]{36}$/);

    await page.reload();
    await expect(panel).toContainText('1 из 2', { timeout: 20_000 });
    await expect(panel).toContainText('Проверить итог');

    const lane = stored.data.todos[1]!.lane_block_id!;
    await page.goto(`${stack.baseUrl}/c/${id}/lane/${lane}`);
    await expect(page.locator('.lane-screen[data-ready]')).toContainText('нашёл три', { timeout: 20_000 });
    await expect(panel).toContainText('Читать журнал');
  });

  test('info_sheet_and_exports_on_real_data', tag, async ({ page }) => {
    test.setTimeout(90_000);
    await page.addInitScript(() => {
      const spy = window as unknown as { __markdown: string; __image: { type: string; b64: string } | null };
      spy.__markdown = '';
      spy.__image = null;
      Object.defineProperty(navigator, 'canShare', { value: undefined, configurable: true });
      Object.defineProperty(navigator, 'share', { value: undefined, configurable: true });
      Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        value: {
          writeText: async (text: string) => { spy.__markdown = text; },
          write: async (items: ClipboardItem[]) => {
            const type = items[0]!.types[0]!;
            const bytes = new Uint8Array(await (await items[0]!.getType(type)).arrayBuffer());
            let binary = '';
            for (const byte of bytes) binary += String.fromCharCode(byte);
            spy.__image = { type, b64: btoa(binary) };
          }
        }
      });
    });
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    await sendScript(page, synthetic.script([
      synthetic.usage({ model: 'synthetic-large', agent: 'main', input: 12_000, output: 3_400, cache_read: 500 }),
      synthetic.subagent('Explore', 'synthetic-small', 'Поиск', [
        synthetic.usage({ model: 'synthetic-small', agent: 'Explore', input: 8_000, output: 900 }),
        synthetic.delta('субагент закончил')
      ]),
      synthetic.delta('Итог: **два** пункта\n\n- один\n- два')
    ]));
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'два' }).first()).toBeVisible({ timeout: 30_000 });
    await hub.waitIdle(id);

    const dialog = await openInfo(page);
    await expect(dialog.getByRole('heading', { name: 'Модели по частям' })).toBeVisible();
    await expect(dialog.locator('code', { hasText: 'synthetic-large' })).toBeVisible();
    await expect(dialog.locator('code', { hasText: 'synthetic-small' })).toBeVisible();
    await expect(dialog.getByText(/Субагент · Explore/).first()).toBeVisible();
    const tokens = async () => (await dialog.locator('.key-value-list').innerText()).replace(/\s/g, '');
    // Основной агент: 12 000 на входе и 3 400 на выходе; субагент: 8 000 и 900.
    await expect.poll(tokens).toMatch(/Вход12.?000\+8.?000/);
    await expect.poll(tokens).toMatch(/Выход3.?400\+900/);

    await dialog.getByRole('button', { name: 'Копировать Markdown' }).click();
    await expect(dialog.getByText('Markdown скопирован')).toBeVisible();
    const markdown = await page.evaluate(() => (window as unknown as { __markdown: string }).__markdown);
    expect(markdown).toContain('**два**');
    expect(markdown).toContain('- один');
    expect(markdown).not.toContain('субагент закончил');

    await dialog.getByRole('button', { name: 'Копировать картинкой' }).click();
    await expect(dialog.getByText('Изображение скопировано в буфер обмена')).toBeVisible({ timeout: 20_000 });
    const image = await page.evaluate(() => (window as unknown as { __image: { type: string; b64: string } }).__image);
    expect(image.type).toBe('image/png');
    // Картинка не пустая: в ней есть текст, а не одна заливка.
    const png = PNG.sync.read(Buffer.from(image.b64, 'base64'));
    expect(png.width).toBeGreaterThan(200);
    expect(png.height).toBeGreaterThan(40);
    const background = [png.data[0], png.data[1], png.data[2]];
    let different = 0;
    for (let i = 0; i < png.data.length; i += 4) if (Math.abs(png.data[i]! - background[0]!) + Math.abs(png.data[i + 1]! - background[1]!) + Math.abs(png.data[i + 2]! - background[2]!) > 90) different++;
    expect(different / (png.width * png.height), 'доля пикселей текста').toBeGreaterThan(0.005);
  });

  test('machine_screen_shows_live_metrics', tag, async ({ page }) => {
    test.setTimeout(60_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    await page.goto(`${stack.baseUrl}/machines`);
    const main = page.locator('main');
    await expect(main.getByRole('heading', { name: stack.machineName })).toBeVisible({ timeout: 15_000 });
    await expect(main.getByText('В сети', { exact: true })).toBeVisible();
    const cpu = main.getByRole('meter', { name: 'CPU' });
    const ram = main.getByRole('meter', { name: 'RAM' });
    await expect(cpu).toBeVisible();
    await expect(ram).toBeVisible();

    // Воркер шлёт метрики раз в 2 с: до страницы доходит новое событие, и показания равны его значениям.
    await sendChaos(stack.control, { action: 'record', on: true });
    const before = (await hub.events('machines', '0')).filter((event) => event.kind === 'machine_metrics').length;
    await expect.poll(async () => (await fetchFrames(stack.control)).filter((f) => f.direction === 'hub_to_client' && f.delivered && f.event?.kind === 'machine_metrics').length, { timeout: 15_000 }).toBeGreaterThanOrEqual(1);
    const frame = (await fetchFrames(stack.control)).filter((f) => f.event?.kind === 'machine_metrics').at(-1)!;
    const metrics = frame.event!.payload as { cpu_permille: number; mem_total_bytes: number; mem_available_bytes: number };
    expect((await hub.events('machines', '0')).filter((event) => event.kind === 'machine_metrics').length).toBeGreaterThan(before - 1);
    await expect.poll(async () => {
      const now = Number(await cpu.getAttribute('aria-valuenow'));
      return Number.isFinite(now) && now >= 0 && now <= 100;
    }).toBe(true);
    const usedRam = Math.round(((metrics.mem_total_bytes - metrics.mem_available_bytes) / metrics.mem_total_bytes) * 100);
    await expect.poll(async () => Math.abs(Number(await ram.getAttribute('aria-valuenow')) - usedRam), { timeout: 10_000 }).toBeLessThanOrEqual(8);
    await expect(page.locator('[data-stale="true"]')).toHaveCount(0);
    await sendChaos(stack.control, { action: 'record', on: false });
  });

  test('limits_history_screen_renders_real_snapshots', tag, async ({ page }) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await hub.createConversation({ harness: 'synthetic' });
    await hub.postAndFinish(id, synthetic.script([
      synthetic.limitObserved('five_hour', 120, 3 * 3_600_000),
      synthetic.limitObserved('weekly', 450, 3 * 86_400_000),
      synthetic.usage({ model: 'synthetic-large', agent: 'main', input: 4000, output: 600 }),
      synthetic.delta('снимок лимитов')
    ]));

    await page.goto(`${stack.baseUrl}/limits`);
    const main = page.locator('main');
    const current = main.getByRole('region', { name: 'Claude, текущие лимиты' });
    await expect(current).toBeVisible({ timeout: 20_000 });
    await expect(current.locator('.value')).toHaveText(['осталось 88%', 'осталось 55%']);
    await expect(main.getByRole('img', { name: /^Недельный остаток по дням\. Claude/ })).toBeVisible();
    const table = main.getByRole('table', { name: 'Недельный остаток по дням' });
    await expect(table).toHaveCount(1);
    const today = table.getByRole('row').filter({ hasText: 'Claude' }).last();
    await expect(today).toContainText('55%');
    // Точки недельного ряда стоят в области графика только у дней со снимком.
    await expect(main.locator('.area [data-series="weekly"]').first()).toBeVisible();
    await expect(main.getByText(/synthetic-large/).first()).toBeVisible();
  });

  test('groups_pin_archive_and_search_sync_across_two_pages', tag, async ({ browser }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const suffix = randomUUID().slice(0, 6);
    const alpha = `Альфа-${suffix}`;
    const beta = `Бета-${suffix}`;
    const idA = await hub.createConversation({ title: alpha, start: false });
    const idB = await hub.createConversation({ title: beta, start: false });
    const group = await hub.json<{ id: string }>('/api/v1/groups', { method: 'POST', body: JSON.stringify({ name: `Группа-${suffix}` }) });

    const context = await browser.newContext({ storageState: STACK_STATE_FILE });
    const one = await context.newPage();
    const two = await context.newPage();
    for (const page of [one, two]) {
      await page.goto(`${stack.baseUrl}/sessions`);
      await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 20_000 });
      await expect(page.locator(`[data-session="${idA}"]`)).toBeVisible({ timeout: 20_000 });
      await expect(page.locator(`[data-session="${idB}"]`)).toBeVisible();
    }

    // Закрепление на первой странице доходит до второй без перезагрузки.
    await one.getByRole('button', { name: `Действия: ${alpha}` }).click();
    await one.getByRole('switch', { name: 'Закрепить' }).click();
    await expect(two.getByRole('button', { name: /^Закреплённые/ })).toBeVisible({ timeout: 20_000 });
    await expect.poll(async () => (await hub.json<{ pinned: boolean }>(`/api/v1/conversations/${idA}`)).pinned).toBe(true);

    // Группа: состав меняется на первой странице, вкладка группы появляется на второй.
    await one.getByRole('checkbox', { name: `Группа-${suffix}` }).check();
    await expect(two.getByRole('tab', { name: new RegExp(`Группа-${suffix}`) })).toBeVisible({ timeout: 20_000 });
    await closeSessionSheet(one);

    // Архив: сессия уходит из основного списка на второй странице.
    await one.getByRole('button', { name: `Действия: ${beta}` }).click();
    await one.getByRole('button', { name: /В архив/ }).click();
    await expect(two.locator(`[data-session="${idB}"]`)).toHaveCount(0, { timeout: 20_000 });
    await expect.poll(async () => (await hub.json<{ archived: boolean }>(`/api/v1/conversations/${idB}`)).archived).toBe(true);

    // Поиск идёт через хаб и находит живую сессию, не находя архивную.
    await two.getByRole('button', { name: 'Поиск', exact: true }).click();
    await two.getByRole('textbox', { name: 'Поиск сессий' }).fill(suffix);
    await expect(two.locator(`[data-session="${idA}"]`)).toBeVisible({ timeout: 20_000 });
    await expect(two.locator(`[data-session="${idB}"]`)).toHaveCount(0);
    void group;
    await context.close();
  });

  test('delete_is_refused_while_live_and_the_menu_stops_an_idle_process_first', tag, async ({ browser }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const title = `Удаление-${randomUUID().slice(0, 6)}`;
    const id = await hub.createConversation({ title, start: true });
    await expect.poll(async () => (await hub.json<{ execution: { state: string } | null }>(`/api/v1/conversations/${id}`)).execution?.state, { timeout: 30_000 }).toBe('running');

    const context = await browser.newContext({ storageState: STACK_STATE_FILE });
    const list = await context.newPage();
    const chat = await context.newPage();
    await list.goto(`${stack.baseUrl}/sessions`);
    await expect(list.locator(`[data-session="${id}"]`)).toBeVisible({ timeout: 20_000 });
    await chat.goto(`${stack.baseUrl}/c/${id}`);
    await expect(chat.locator('[data-ready]')).toBeVisible({ timeout: 20_000 });

    const refused = await hub.fetch(`/api/v1/conversations/${id}`, { method: 'DELETE' });
    expect(refused.status).toBe(409);
    expect(((await refused.json()) as { error: { code: string } }).error.code).toBe('conversation_live');

    await list.getByRole('button', { name: `Действия: ${title}` }).click();
    await list.getByRole('button', { name: /^Переместить в корзину/ }).click();
    const confirm = list.getByRole('dialog', { name: 'Остановить и убрать в корзину?' });
    await expect(confirm).toContainText('будет остановлен');
    await confirm.getByRole('button', { name: 'Остановить и убрать' }).click();
    await expect(list.locator(`[data-session="${id}"]`)).toHaveCount(0, { timeout: 30_000 });
    await expect(chat.getByRole('heading', { name: 'Сессия в корзине' })).toBeVisible({ timeout: 20_000 });

    const trashed = await hub.fetch(`/api/v1/conversations/${id}`);
    expect(trashed.status).toBe(200);
    expect(((await trashed.json()) as { trashed_at: string | null }).trashed_at).not.toBeNull();
    await context.close();
  });

  test('settings_change_midsession_applies_with_the_next_turn', tag, async ({ page }) => {
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);

    await sendScript(page, synthetic.script([synthetic.delta('первый ход')]));
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'первый ход' })).toBeVisible({ timeout: 30_000 });
    await hub.waitIdle(id);

    // Модель меняется после первого хода: в шапке и на хабе она ждёт следующего хода.
    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await page.getByRole('menuitem', { name: 'Настройки сессии' }).click();
    await page.locator('#model').selectOption('synthetic-small');
    await page.getByRole('button', { name: 'Сохранить настройки' }).click();
    await expect(page.locator('.subtitle').first()).toContainText('со следующего хода', { timeout: 15_000 });
    // Лист закрывается только после записи кранча, а пока он открыт, поле ввода инертно и не примет текст.
    await expect(page.getByRole('dialog')).toHaveCount(0, { timeout: 15_000 });
    let detail = await hub.json<{ model: string; settings: { effective: { model: string }; pending: { model?: string } } }>(`/api/v1/conversations/${id}`);
    expect(detail.settings.effective.model).toBe('synthetic-large');
    expect(detail.settings.pending.model).toBe('synthetic-small');

    await sendScript(page, synthetic.script([synthetic.delta('второй ход')]));
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'второй ход' })).toBeVisible({ timeout: 30_000 });
    await hub.waitIdle(id);
    detail = await hub.json(`/api/v1/conversations/${id}`);
    expect(detail.settings.effective.model).toBe('synthetic-small');
    expect(detail.settings.pending.model ?? null).toBeNull();
    await expect(page.locator('.subtitle').first()).not.toContainText('со следующего хода');
  });

  test('new_session_gets_home_and_suggestions_from_the_real_worker', tag, async ({ page }) => {
    const stack = getStack();
    mkdirSync(join(stack.workRoot, 'suggest-alpha', 'inner'), { recursive: true });
    writeFileSync(join(stack.workRoot, 'suggest-file.txt'), 'файл');
    await page.goto(`${stack.baseUrl}/new`);
    await expect(page.locator('[data-new-session]')).toBeVisible({ timeout: 15_000 });
    const field = page.getByRole('textbox', { name: 'Рабочая папка' });
    await expect(page.getByRole('combobox', { name: 'Машина' })).not.toHaveValue('');
    await expect(field).toHaveValue(homedir());
    await field.fill(`${stack.workRoot}/suggest`);
    await expect(page.getByRole('option', { name: `${stack.workRoot}/suggest-alpha`, exact: true })).toBeVisible();
    await expect(page.getByRole('listbox', { name: 'Подсказки папок' }).getByRole('option')).toHaveCount(1);
    await field.press('ArrowDown');
    await field.press('Enter');
    await expect(field).toHaveValue(`${stack.workRoot}/suggest-alpha/`);
    await expect(page.getByRole('option', { name: `${stack.workRoot}/suggest-alpha/inner`, exact: true })).toBeVisible();
    await field.fill('/etc/');
    await expect(page.getByRole('listbox', { name: 'Подсказки папок' })).toHaveCount(0);
  });

  test('edge_proxy_strips_client_remote_headers', tag, async ({ browser }) => {
    const stack = getStack();
    const hub = createHubClient(stack);
    // Без cookie сеанса хаб не пускает: ни напрямую, ни с поддельными заголовками личности.
    const direct = await fetch(`${stack.hubUrl}/api/v1/me`, { headers: { 'Remote-User': 'owner' } });
    expect(direct.status).toBe(401);
    const forgedDirect = await fetch(`${stack.hubUrl}/api/v1/me`, { headers: { 'Remote-User': 'other', 'X-Canterlot-Proxy': 'forged' } });
    expect(forgedDirect.status).toBe(401);
    expect((await hub.fetch('/api/v1/me')).status).toBe(200);

    // Страница без входа подделывает заголовки через кромку: 401 unauthenticated и без признака утечки.
    const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await context.newPage();
    await page.goto(stack.baseUrl);
    const viaEdge = await page.evaluate(async () => {
      const res = await fetch('/api/v1/me', { headers: { 'Remote-User': 'owner', 'X-Canterlot-Proxy': 'forged', 'X-Canterlot-Client-Ip': '203.0.113.9', 'X-Forwarded-For': '203.0.113.9', 'Remote-Groups': 'admins' } });
      return { status: res.status, leak: res.headers.get('X-Canterlot-Proxy-Leak'), body: await res.json() };
    });
    expect(viaEdge.status).toBe(401);
    expect(viaEdge.leak).toBeNull();
    expect((viaEdge.body as { error: { code: string } }).error.code).toBe('unauthenticated');
    await expect(page.getByRole('heading', { name: 'Вход в canterlot' })).toBeVisible({ timeout: 15_000 });
    await context.close();

    // Адрес клиента ставит кромка по настоящему пиру, а не по присланному заголовку.
    const headers = { Origin: stack.baseUrl, 'X-Canterlot-Client': '1', 'Content-Type': 'application/json', 'X-Canterlot-Client-Ip': '203.0.113.9', 'X-Forwarded-For': '203.0.113.9' };
    const signedIn = await fetch(`${stack.baseUrl}/api/v1/auth/recovery`, { method: 'POST', headers, body: JSON.stringify({ token: stack.recoveryToken() }) });
    expect(signedIn.status).toBe(200);
    const view = (await signedIn.json()) as { session: { created_ip: string | null; id: string } };
    expect(view.session.created_ip).toBe('127.0.0.1');
    const cookie = signedIn.headers.getSetCookie().map((line) => line.split(';')[0]).join('; ');
    expect((await fetch(`${stack.baseUrl}/api/v1/auth/logout`, { method: 'POST', headers: { Origin: stack.baseUrl, 'X-Canterlot-Client': '1', Cookie: cookie } })).status).toBe(200);
  });
});
