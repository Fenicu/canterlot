import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { DEEPSEEK, seedRoutedSpawn, seedRoutingCatalog, seedRoutingWhitelist } from '../../testing/fixtures/routing';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d3';
const N = ' ';

const catalog = seedRoutingCatalog;

async function bootEditor(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  catalog(api);
  seedRoutingWhitelist(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/settings/routing');
  await expect(page.locator('[data-routing-editor][data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

type Saved = { task_types: Record<string, { title: string; candidates: { harness: string; model: string }[] }> };
const savedModels = (api: FakeHubApi, key: string) => (api.routingPolicy as Saved).task_types[key]!.candidates.map((c) => c.model);

test.describe('редактор маршрутизации', () => {
  test('editor_loads_reorders_by_keyboard_and_saves_with_revision', async ({ page }) => {
    const { api } = await bootEditor(page);
    await expect(page.locator('[data-role="policy-status"]')).toContainText('Действует встроенная политика');
    const bugfix = page.locator('[data-task-key="bugfix"]');
    await expect(bugfix.locator('[data-candidate]')).toHaveCount(2);
    // Opus не в белом списке: подсвечен, но сохраняется.
    await expect(bugfix.locator('[data-pair="claude_code/opus[1m]"] [data-role="not-whitelisted"]')).toContainText('не в белом списке — маршрут их пропустит', { ignoreCase: true });
    await expect(bugfix.locator(`[data-pair="opencode/${DEEPSEEK}"] [data-role="not-whitelisted"]`)).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Сохранить' })).toBeDisabled();

    const down = page.getByRole('button', { name: 'Опустить DeepSeek' });
    await down.focus();
    await page.keyboard.press('Enter');
    await expect(bugfix.locator('[data-candidate]').first()).toHaveAttribute('data-pair', 'claude_code/opus[1m]');
    await bugfix.getByLabel('Название типа bugfix').fill('Исправление');
    await page.getByRole('button', { name: 'Сохранить' }).click();
    await expect(page.locator('[data-role="policy-status"]')).toContainText('Своя политика · ревизия 1');
    expect(api.routingPolicyRevision).toBe(1);
    expect(savedModels(api, 'bugfix')).toEqual(['opus[1m]', DEEPSEEK]);
    expect((api.routingPolicy as Saved).task_types.bugfix!.title).toBe('Исправление');
    await expect(page.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
  });

  test('candidate_drags_by_the_grip_and_the_order_is_saved', async ({ page }) => {
    const { api } = await bootEditor(page);
    const bugfix = page.locator('[data-task-key="bugfix"]');
    const grip = page.getByRole('button', { name: 'Перетащить Opus 1M' });
    const first = bugfix.locator(`[data-pair="opencode/${DEEPSEEK}"]`);
    await grip.scrollIntoViewIfNeeded();
    const from = (await grip.boundingBox())!;
    const to = (await first.boundingBox())!;
    await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
    await page.mouse.down();
    await page.mouse.move(from.x + from.width / 2, to.y + 4, { steps: 8 });
    await page.mouse.up();
    await expect(bugfix.locator('[data-candidate]').first()).toHaveAttribute('data-pair', 'claude_code/opus[1m]');
    await page.getByRole('button', { name: 'Сохранить' }).click();
    await expect.poll(() => api.routingPolicyRevision).toBe(1);
    expect(savedModels(api, 'bugfix')).toEqual(['opus[1m]', DEEPSEEK]);
  });

  test('validation_blocks_save_with_russian_messages', async ({ page }) => {
    await bootEditor(page);
    const bugfix = page.locator('[data-task-key="bugfix"]');
    await bugfix.getByLabel('Ключ типа Исправление по ясному брифу').fill('Bad Key');
    await expect(page.getByText('Ключ: строчные латинские буквы, цифры и «_», до 32 знаков')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Сохранить' })).toBeDisabled();
    await expect(page.getByText('Исправьте ошибки выше: 1')).toBeVisible();
  });

  test('stale_revision_answers_409_and_the_reload_prompt_restores_the_saved_policy', async ({ page }) => {
    const { api } = await bootEditor(page);
    const bugfix = page.locator('[data-task-key="bugfix"]');
    await bugfix.getByLabel('Название типа bugfix').fill('Моё название');
    // Другое окно сохранило политику, событие ещё не пришло.
    api.routingPolicy = { ...api.builtinRoutingPolicy, task_types: { ...api.builtinRoutingPolicy.task_types, bugfix: { ...api.builtinRoutingPolicy.task_types.bugfix, title: 'Из другого окна' } } };
    api.routingPolicyRevision = 1;
    await page.getByRole('button', { name: 'Сохранить' }).click();
    const conflict = page.locator('[data-role="policy-conflict"]');
    await expect(conflict).toContainText('Политику изменили в другом окне');
    await conflict.getByRole('button', { name: 'Загрузить заново' }).click();
    await expect(conflict).toHaveCount(0);
    await expect(bugfix.getByLabel('Название типа bugfix')).toHaveValue('Из другого окна');
    await expect(page.locator('[data-role="policy-status"]')).toContainText('ревизия 1');
  });

  test('routing_policy_changed_reloads_a_clean_editor_and_warns_a_dirty_one', async ({ page }) => {
    const { hub, api } = await bootEditor(page);
    await expect.poll(() => hub.subscribed('conversations'), { timeout: 15_000 }).toBe(true);
    const own = { ...api.builtinRoutingPolicy, task_types: { review: api.builtinRoutingPolicy.task_types.review } };
    api.saveRoutingPolicy(own, 0);
    hub.flush();
    await expect(page.locator('[data-task-key="bugfix"]')).toHaveCount(0);
    await expect(page.locator('[data-role="policy-status"]')).toContainText('ревизия 1');
    await page.locator('[data-task-key="review"]').getByLabel('Название типа review').fill('Правка');
    api.saveRoutingPolicy(api.builtinRoutingPolicy, 1);
    hub.flush();
    await expect(page.locator('[data-role="policy-conflict"]')).toContainText('Политику изменили в другом окне');
  });

  test('reset_to_builtin_asks_and_sends_null_policy', async ({ page }) => {
    const hub = new FakeHub();
    const api = new FakeHubApi(hub);
    catalog(api);
    api.routingPolicy = { ...api.builtinRoutingPolicy, task_types: { review: api.builtinRoutingPolicy.task_types.review } };
    api.routingPolicyRevision = 4;
    await installChatRoutes(page, hub, api, OWNER);
    await page.goto('/settings/routing');
    await expect(page.locator('[data-role="policy-status"]')).toContainText('Своя политика · ревизия 4');
    await page.getByRole('button', { name: 'Сбросить к встроенной' }).click();
    await expect(page.getByText('Своя политика будет удалена, маршрут пойдёт по встроенной раскладке')).toBeVisible();
    await page.getByRole('button', { name: 'Сбросить', exact: true }).click();
    await expect(page.locator('[data-role="policy-status"]')).toContainText('Действует встроенная политика');
    expect(api.routingPolicy).toBeNull();
    expect(api.routingPolicyRevision).toBe(5);
    await expect(page.locator('[data-task-key="bugfix"]')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Сбросить к встроенной' })).toBeDisabled();
  });

  test('preview_shows_the_choice_with_translated_verdicts_and_the_guard', async ({ page }) => {
    const { api } = await bootEditor(page);
    api.routingSkips.set(`opencode/${DEEPSEEK}`, { verdict: 'parallel_busy', reason: `4 of 4 ${DEEPSEEK} sessions busy` });
    api.routingGuard = { stale: false, blocked: true, violations: ['disk 12 GB free < 30 GB'], detail: 'disk 12 GB free < 30 GB.' };
    const preview = page.locator('[data-role="routing-preview"]');
    await preview.getByLabel('Уровень').selectOption('high');
    await preview.getByRole('button', { name: 'Проверить' }).click();
    const result = preview.locator('[data-role="preview-result"]');
    await expect(result.locator('[data-role="route-chosen"]')).toHaveText('Opus 1M · высокое');
    await expect(result).toContainText('занят');
    await expect(result).toContainText('занято 4 из 4 сессий deepseek-v4.1-flash');
    await expect(result.locator('[data-role="preview-guard"]')).toHaveText(`Страж: запуск встанет в очередь — диск: свободно 12${N}ГБ при минимуме 30${N}ГБ`);
    await expect(result.locator('[data-role="preview-machine"]')).toContainText('диск свободно 72');
    expect(api.routingPreviews.at(-1)).toMatchObject({ task_type: 'bugfix', level: 'high' });

    const reset = new Date(Date.now() + 2 * 3_600_000);
    api.routingRefusal = { candidates: [{ harness: 'codex', model: 'gpt-6.1-sol', verdict: 'limits_over', reason: 'codex weekly 83% > 80%' }], nearest_reset: reset.toISOString() };
    await preview.getByLabel('Тип задачи').selectOption('review');
    await preview.getByRole('button', { name: 'Проверить' }).click();
    const refused = preview.locator('[data-role="preview-refused"]');
    await expect(refused.locator('li')).toHaveText(`Codex — неделя 83${N}% при пороге 80${N}%`);
    await expect(refused).toContainText(`Ближайший сброс лимита в ${new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit' }).format(reset)}`);
  });
});

async function bootOrchestrator(page: Page, prepare?: (api: FakeHubApi, id: string) => void) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  catalog(api);
  const id = seedConversation(api, { title: 'Оркестратор', harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' } });
  prepare?.(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

test.describe('очередь запусков и маршрут в чате', () => {
  test('queue_plaque_appears_on_event_and_cancel_and_start_now_call_the_api', async ({ page }) => {
    const { hub, api, id } = await bootOrchestrator(page);
    const plaque = page.locator('[data-role="spawn-queue"]');
    await expect(plaque).toHaveCount(0);
    const soon = new Date(Date.now() + 10 * 60_000);
    const first = api.queueItem(id, { title: 'Исправить гонку', task_type: 'bugfix', level: 'high', reason: 'rate', detail: { detail: '10 spawns in the last hour; the next slot opens at 14:32 UTC' }, not_before: soon.toISOString(), created_at: '2026-11-06T10:00:00.000Z' });
    const second = api.queueItem(id, { harness: 'codex', model: 'gpt-6.1-sol', reason: 'concurrency', detail: { detail: `2 of 2 ${DEEPSEEK} sessions busy` }, not_before: new Date(Date.now() + 20 * 60_000).toISOString(), created_at: '2026-11-06T10:01:00.000Z', position: 2 });
    hub.flush();
    const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit' }).format(soon);
    await expect(plaque.locator('[data-role="spawn-queue-summary"]')).toHaveText(`В очереди запуска: 2 · следующий в ${clock}`);
    await plaque.getByRole('button', { name: /В очереди запуска/ }).click();
    const one = plaque.locator(`[data-queue-item="${first.id}"]`);
    await expect(one.locator('[data-role="queue-title"]')).toHaveText('Исправить гонку');
    await expect(one.locator('[data-role="queue-target"]')).toHaveText('маршрут «Исправление по ясному брифу» · высокое');
    await expect(plaque.locator(`[data-queue-item="${second.id}"] [data-role="queue-title"]`)).toHaveText('Запуск без названия');
    await expect(plaque.locator(`[data-queue-item="${second.id}"] [data-role="queue-target"]`)).toHaveText('GPT-6.1 Sol');
    await expect(one.locator('[data-role="queue-reason"]')).toContainText('частота запусков: 10 запусков за час, следующий в');
    await expect(plaque.locator(`[data-queue-item="${second.id}"] [data-role="queue-reason"]`)).toHaveText('исполнители заняты: занято 2 из 2 сессий deepseek-v4.1-flash');

    await one.getByRole('button', { name: 'Отменить' }).click();
    await expect(one).toHaveCount(0);
    expect(api.spawnQueue.get(id)?.find((item) => item.id === first.id)?.state).toBe('cancelled');
    await expect(plaque.locator('[data-role="spawn-queue-summary"]')).toContainText('В очереди запуска: 1');

    await plaque.locator(`[data-queue-item="${second.id}"]`).getByRole('button', { name: 'Запустить сейчас' }).click();
    await expect(page.getByText('Снимает лимиты и страж, но не белый список')).toBeVisible();
    await page.getByRole('dialog').getByRole('button', { name: 'Запустить сейчас' }).click();
    await expect(plaque).toHaveCount(0);
    expect(api.spawnQueue.get(id)?.find((item) => item.id === second.id)?.state).toBe('started');
  });

  test('reconnect_restores_the_queue_from_the_snapshot', async ({ page }) => {
    const { hub, api, id } = await bootOrchestrator(page);
    await expect(page.locator('[data-role="spawn-queue"]')).toHaveCount(0);
    // Пока связи нет, хаб поставил запуск в очередь: событие пропущено, снимок его знает.
    hub.acceptConnections = false;
    hub.chaos.cut();
    const now = new Date().toISOString();
    api.spawnQueue.set(id, [{ id: '00000000-0000-4000-8000-0000000000f1', parent_id: id, reason: 'resources', detail: { detail: 'memory 2 GB available < 4 GB' }, state: 'queued', not_before: now, expires_at: new Date(Date.now() + 86_400_000).toISOString(), attempts: 0, child_id: null, result: null, created_at: now, updated_at: now, position: 1 }]);
    hub.acceptConnections = true;
    await page.evaluate(() => window.dispatchEvent(new Event('online')));
    await expect.poll(() => hub.activeConnections > 0 && hub.subscribed(`conversation:${id}`), { timeout: 15_000 }).toBe(true);
    hub.chaos.forceResync('cursor_too_old');
    await expect(page.locator('[data-role="spawn-queue-summary"]')).toHaveText('В очереди запуска: 1 · следующий сейчас', { timeout: 15_000 });
  });

  test('queued_spawn_is_a_pale_tree_node_that_grows_into_the_real_session', async ({ page }) => {
    const { hub, api, id } = await bootOrchestrator(page);
    const item = api.queueItem(id, { reason: 'limits', detail: { detail: 'quota paused until 18:40 UTC' } });
    hub.flush();
    const desktop = test.info().project.name === 'desktop';
    if (desktop) await page.getByRole('button', { name: 'Делегирование', exact: true }).click();
    else await page.getByRole('button', { name: 'Делегирование' }).click();
    const pale = page.locator(`[data-queue-node="${item.id}"]`);
    await expect(pale).toContainText('Запуск без названия');
    await expect(pale).toContainText('лимиты провайдера: квота на паузе до');
    await expect(pale.getByRole('group', { name: 'Запуск из очереди' })).toBeVisible();
    await expect(pale.locator('[aria-current="step"]')).toContainText('В очереди');

    hub.append(`conversation:${id}`, { kind: 'spawn_queue_changed', conversation_id: id, payload: { ...item, state: 'starting' } });
    hub.flush();
    await expect(pale.locator('[aria-current="step"]')).toContainText('Запускается');

    const child = api.id();
    api.queueItem(id, { ...item, state: 'started', child_id: child });
    hub.flush();
    await expect(pale.locator('[aria-current="step"]')).toContainText('Создана');
    seedConversation(api, { id: child, title: 'Фикс из очереди', parent_id: id, spawn_depth: 1 });
    hub.flush();
    await expect(page.locator(`[data-node="${child}"]`)).toContainText('Фикс из очереди');
    await expect(pale).toHaveCount(0);
  });

  test('spawn_line_names_the_route_and_the_delegate_header_opens_the_decision', async ({ page }) => {
    let child = '';
    await bootOrchestrator(page, (api, parent) => {
      child = seedRoutedSpawn(api, parent);
    });
    await expect(page.locator('[data-role="spawn-route-line"]')).toHaveText(`Маршрут «Исправление по ясному брифу»: DeepSeek · высокое. Пропущен: Codex — неделя 83${N}% при пороге 80${N}%`);
    await expect(page.locator('[data-role="spawn-queued-line"]')).toContainText('Запуск в очереди: частота запусков: 10 запусков за час');
    await expect(page.locator('[data-role="spawn-metrics-stale"]')).toHaveText('Метрики машины устарели: страж не проверял запас');

    await page.goto(`/c/${child}`);
    const line = page.locator('[data-role="route-state-line"]');
    await expect(line).toHaveText('Выбрана маршрутом «Исправление по ясному брифу»');
    await line.click();
    const sheet = page.getByRole('dialog', { name: 'Выбор маршрута' });
    await expect(sheet.locator('[data-role="route-chosen"]')).toHaveText('DeepSeek · высокое');
    await expect(sheet.locator('[data-verdict="limits_over"]')).toContainText(`выше порога`);
    await expect(sheet.locator('[data-verdict="limits_over"]')).toContainText(`Codex — неделя 83${N}% при пороге 80${N}%`);
    await expect(sheet).toContainText('до 2 толчков');
    await expect(sheet).toContainText('90 мин');
  });

  test('start_now_conflict_shows_the_reason_and_rereads_the_queue', async ({ page }) => {
    const { hub, api, id } = await bootOrchestrator(page);
    const item = api.queueItem(id, { title: 'Ждёт машину', reason: 'resources', detail: { detail: 'disk 12 GB free < 30 GB.' } });
    hub.flush();
    api.startNowRefusal = 'disk 10 GB free < 30 GB.';
    const plaque = page.locator('[data-role="spawn-queue"]');
    await plaque.getByRole('button', { name: /В очереди запуска/ }).click();
    const row = plaque.locator(`[data-queue-item="${item.id}"]`);
    await row.getByRole('button', { name: 'Запустить сейчас' }).click();
    const reread = page.waitForRequest((request) => request.method() === 'GET' && /\/spawn-queue$/.test(new URL(request.url()).pathname));
    await page.getByRole('dialog').getByRole('button', { name: 'Запустить сейчас' }).click();
    await reread;
    await expect(row.getByRole('alert')).toHaveText(`Запустить сейчас не удалось: диск: свободно 10${N}ГБ при минимуме 30${N}ГБ`);
    expect(api.spawnQueue.get(id)![0]!.attempts).toBe(1);
  });
});
