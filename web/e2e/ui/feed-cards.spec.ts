import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { TODO_SECOND, hubText, seedFeedCards, setActivity, streamCall, type FeedCardsSeed } from '../../testing/fixtures/feed-cards';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c1';
const PNG = readFileSync(resolve(import.meta.dirname, '../../../docs/screenshots/chat-dark.png'));

async function boot(page: Page, opts: { working?: boolean } = {}): Promise<{ hub: FakeHub; api: FakeHubApi; seed: FeedCardsSeed }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedFeedCards(api);
  if (opts.working) api.details.get(seed.id)!.activity = { state: 'working', evidence: {}, since: new Date().toISOString() };
  await installChatRoutes(page, hub, api, OWNER);
  await page.route(`**/api/v1/attachments/${seed.imageId}/content`, (route) => route.fulfill({ contentType: 'image/png', body: PNG }));
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, seed };
}

const feed = (page: Page) => page.locator('.feed');
const row = (page: Page, id: string) => page.locator(`[data-key="block:${id}"]`);
const card = (page: Page, id: string) => row(page, id).locator('.feed-card').first();

/** Лента виртуальная: строка далеко от низа не смонтирована, пока до неё не докрутили. */
async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await feed(page).evaluate((node) => {
    node.scrollTop = 0;
  });
  await settle();
  for (let step = 0; step < 60; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle();
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили: ищем дальше.
      }
    }
    await feed(page).evaluate((node) => {
      node.scrollTop += node.clientHeight * 0.6;
    });
    await settle();
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

async function atBottom(page: Page): Promise<number> {
  return feed(page).evaluate((node) => node.scrollHeight - node.clientHeight - node.scrollTop);
}

test('every_card_renders_from_a_seeded_orchestrator_feed', async ({ page }) => {
  // Каждая карточка ищется прокруткой виртуальной ленты: проходов много.
  test.setTimeout(120_000);
  const { seed } = await boot(page);
  const expected: [string, string, RegExp | string][] = [
    ['todo1', 'todo.write', /Задачи/],
    ['limits', 'canterlot.limits', /Лимиты/],
    ['spawn', 'canterlot.session_spawn', 'Запустила «Карточки ленты»'],
    ['spawnRouted', 'canterlot.session_spawn', 'Запустила «Фикс скролла»'],
    ['spawnPending', 'canterlot.session_spawn', 'Попросила разрешения на запуск «Эксперимент с Gemini»'],
    ['spawnQueued', 'canterlot.session_spawn', 'Поставила в очередь «Скриншоты тем»'],
    ['list', 'canterlot.sessions_list', 'Сессии: 1 работает, 1 ждёт ответа, 1 ждёт сообщения, 1 в очереди запуска'],
    ['inspect', 'canterlot.session_inspect', 'Посмотрела «Фикс скролла» · задала вопрос'],
    ['send', 'canterlot.session_send', 'Написала «Фикс скролла»'],
    ['alarm', 'canterlot.alarm_set', /^Будильник · когда «Карточки ленты» и «Фикс скролла» закончат · до \d{2}:\d{2}$/],
    ['alarmFile', 'canterlot.alarm_set', /^Будильник · «gate» · когда появится \/tmp\/gate\.rc · до \d{2}:\d{2}$/],
    ['alarmList', 'canterlot.alarm_list', /^Будильники: 2 взведены · ближайший в \d{2}:\d{2}$/],
    ['skill', 'skill', 'Навык: brainstorming'],
    ['search', 'web.search', 'Искала: tool call cards chat feed design · 3 ссылки'],
    ['fetch', 'web.fetch', 'Открыла svelte.dev · <svelte:boundary>'],
    ['docs', 'context7.query-docs', 'Документация: svelte — svelte:boundary failed snippet'],
    ['taiga', 'taiga.userstory_update', 'Taiga · обновила историю #123 «Карточки инструментов в ленте»'],
    ['taigaComment', 'taiga.comment_add', 'Taiga · комментарий к задаче #45'],
    ['todo2', 'todo.write', /Задачи/],
    ['share', 'canterlot.share_file', 'Передала файл «Карточки в ленте»'],
    ['accept', 'canterlot.session_accept', 'Приняла «Миграция лимитов»'],
    ['nudge', 'canterlot.nudge_set', 'Подталкивание «Карточки ленты» · до 2 раз подряд, после 5 мин тишины'],
    ['stop', 'canterlot.session_stop', 'Остановила «Ревью вёрстки»']
  ];
  for (const [name, id, text] of expected) {
    const found = await reveal(page, card(page, seed.blocks[name]!));
    await expect(found, name).toHaveAttribute('data-card', id);
    await expect(found.locator('.head .lead'), name).toHaveText(text);
  }
  // Ссылки ведут в сессии и на страницы, а не в сырой вызов.
  await expect((await reveal(page, card(page, seed.blocks.spawn!))).locator('.head a')).toHaveAttribute('href', `/c/${seed.child}`);
  await expect((await reveal(page, card(page, seed.blocks.taiga!))).locator('.head a')).toHaveAttribute('href', 'https://taiga.example/project/canterlot/us/123');
  const spawn = await reveal(page, card(page, seed.blocks.spawn!));
  await expect(spawn.locator('[data-role="spawn-meta"]')).toHaveText('Opus 1M · высокое · отдельная копия, ветка canterlot/8b993844');
  await expect(spawn.locator('[data-role="spawn-brief"]')).toContainText('Собери карточки ленты');
  await spawn.locator('[data-role="spawn-brief"]').click();
  await expect(spawn.locator('[data-role="spawn-brief-full"]')).toContainText('Не трогай MediaViewer.svelte');
  const queued = await reveal(page, card(page, seed.blocks.spawnQueued!));
  await expect(queued.locator('[data-role="spawn-queued-line"]')).toContainText('Запуск в очереди: частота запусков');
  await expect(queued.locator('[data-role="card-state"]')).toHaveText(/^следующий в \d{2}:\d{2}$/);
  await expect((await reveal(page, card(page, seed.blocks.spawnPending!))).locator('[data-role="card-state"]')).toHaveText('ждёт владельца');
  await expect((await reveal(page, card(page, seed.blocks.accept!))).locator('[data-role="accept-facts"]')).toHaveText('3 коммита · рабочая копия убрана · ветка canterlot/5e1f0a2b оставлена');
  await expect((await reveal(page, card(page, seed.blocks.accept!))).locator('[data-role="card-state"]')).toHaveText('остановлена');
  const limits = await reveal(page, card(page, seed.blocks.limits!));
  await expect(limits.locator('[data-provider="codex"] [data-window="week"] .pct')).toHaveText(/^осталось\s17%$/);
  await expect(limits.locator('[data-provider="codex"] [data-window="week"] .pct')).toHaveAttribute('data-tone', 'warning');
  await expect(limits.locator('[data-provider="opencode"] [data-window="five"] .pct')).toHaveAttribute('data-tone', 'danger');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test('spawn_card_shows_the_live_child_state_and_follows_the_hub', async ({ page }) => {
  const { hub, api, seed } = await boot(page);
  const spawn = await reveal(page, card(page, seed.blocks.spawn!));
  const state = spawn.locator('[data-role="card-state"]');
  await expect(state).toHaveText('работает');
  await expect(state).toHaveAttribute('data-tone', 'accent');
  setActivity(hub, api, seed.child, 'waiting_for_user', { question: 'Какой порог?' });
  await expect(state).toHaveText('задала вопрос');
  await expect(state).toHaveAttribute('data-tone', 'warning');
  setActivity(hub, api, seed.child, 'idle');
  await expect(state).toHaveText('завершена');
  await expect(state).toHaveAttribute('data-tone', 'success');
});

test('todo_card_shows_what_changed_since_the_previous_list', async ({ page }) => {
  const { seed } = await boot(page);
  const todo = await reveal(page, card(page, seed.blocks.todo2!));
  await expect(todo.locator('[data-role="card-state"]')).toHaveText('2 из 5');
  await expect(todo.locator('[data-role="todo-delta"]')).toHaveText('закрыт пункт');
  await expect(todo.locator('.item[data-done]')).toHaveText(/Реестр карточек.*готово/);
  await expect(todo.locator('.item[data-added]')).toHaveText(/Скриншоты для владельца.*новый/);
  await expect(todo.locator('.item[data-status="in_progress"]')).toHaveText(/Карточка запуска делегата.*в работе/);
  // Без галочек: состояние словом и цветом.
  await expect(todo).not.toContainText(/[✓✔☑✅]/u);
  await todo.getByRole('button', { name: 'Весь список · 5' }).click();
  await expect(todo.locator('.item')).toHaveCount(5);
});

test('a_todo_update_that_arrives_live_strikes_the_closed_item', async ({ page }) => {
  const { hub, api, seed } = await boot(page, { working: true });
  const next = { todos: TODO_SECOND.todos.map((item) => (item.content === 'Карточка запуска делегата' ? { ...item, status: 'completed' } : item.content === 'Тесты и видео' ? { ...item, status: 'in_progress' } : item)) };
  const fresh = streamCall(hub, api, seed.id, seed.run, 'TodoWrite', next, { result: 'Todos have been modified successfully.' });
  const todo = card(page, fresh.call);
  await expect(todo).toBeVisible();
  const done = todo.locator('.item[data-done]');
  await expect(done).toHaveText(/Карточка запуска делегата.*готово/);
  // Черта доводится до конца: у закрытого на глазах пункта она проведена, а не стоит сразу.
  await expect.poll(() => done.locator('.strike').evaluate((node) => getComputedStyle(node).clipPath)).toMatch(/inset\(0(px)?\)|none/);
  await expect(todo.locator('[data-role="card-state"]')).toHaveText('3 из 5');
});

test('share_file_card_opens_the_media_viewer', async ({ page }) => {
  const { seed } = await boot(page);
  const share = await reveal(page, card(page, seed.blocks.share!));
  await expect(share.locator('[data-role="share-note"]')).toHaveText('Телефон, тёмная тема: запуск, задачи, лимиты.');
  await share.locator('button.thumb').click();
  const dialog = page.locator('dialog[open]');
  await expect(dialog).toHaveAttribute('aria-label', 'Файл');
  await expect(dialog.locator('img')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
});

test('quiet_meta_calls_fold_into_one_muted_line', async ({ page }) => {
  const { seed } = await boot(page);
  const group = await reveal(page, page.locator('[data-role="quiet-group"]'));
  await expect(group.locator('.line')).toHaveText('Служебные вызовы · 2 · Поиск инструментов, Наблюдение');
  await expect(row(page, seed.blocks.toolsearch!)).toHaveCount(0);
  await group.locator('.head').click();
  await expect(card(page, seed.blocks.toolsearch!)).toHaveAttribute('data-quiet', '');
  await expect(card(page, seed.blocks.toolsearch!).locator('.lead')).toHaveText('Поиск инструментов: mcp__canterlot__session_spawn');
  await expect(card(page, seed.blocks.monitor!).locator('.lead')).toHaveText('Наблюдение: ждёт сборку web');
});

test('every_card_expands_to_the_raw_call_and_result', async ({ page }) => {
  const { seed } = await boot(page);
  const limits = await reveal(page, card(page, seed.blocks.limits!));
  await expect(limits.locator('[data-role="card-raw"]')).toHaveCount(0);
  await limits.locator('.hit').click();
  await expect(limits.locator('.hit')).toHaveAttribute('aria-expanded', 'true');
  await expect(limits.locator('[data-role="card-raw"]')).toContainText('Claude API budget: five-hour window 15% used');
  const spawn = await reveal(page, card(page, seed.blocks.spawn!));
  // Середину шапки занимает ссылка на делегата: раскрытие — по шеврону.
  await spawn.locator('.hit').click({ position: { x: 12, y: 12 } });
  const raw = spawn.locator('[data-role="card-raw"]');
  await expect(raw).toContainText('"isolated":true');
  await expect(raw).toContainText('"status": "started"');
});

test('generic_tools_and_unknown_tools_keep_the_plain_block', async ({ page }) => {
  const { seed } = await boot(page);
  const unknown = await reveal(page, row(page, seed.blocks.unknown!).locator('.tool'));
  await expect(unknown.locator('.name')).toHaveText('create_issue');
  await expect(row(page, seed.blocks.unknown!).locator('.feed-card')).toHaveCount(0);
  const bash = await reveal(page, row(page, seed.blocks.bash!).locator('.tool'));
  await expect(bash.locator('.name')).toHaveText('Bash');
  await expect(bash.locator('.summary')).toHaveText('npm run lint:tokens');
});

test('malformed_input_of_a_known_tool_falls_back_to_the_plain_block', async ({ page }) => {
  const { hub, api, seed } = await boot(page, { working: true });
  const broken = streamCall(hub, api, seed.id, seed.run, 'TodoWrite', { todos: 'не список' }, { result: 'ok' });
  await expect(row(page, broken.call).locator('.tool .name')).toHaveText('TodoWrite');
  await expect(row(page, broken.call).locator('.feed-card')).toHaveCount(0);
});

for (const motion of ['full', 'reduced'] as const) {
  test(`feed_stays_pinned_at_the_bottom_while_cards_mount_${motion}`, async ({ page }) => {
    if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
    const { hub, api, seed } = await boot(page, { working: true });
    await expect(feed(page)).toHaveAttribute('data-pinned', '');
    await expect.poll(() => atBottom(page)).toBeLessThanOrEqual(2);
    const fresh = streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__session_spawn', { title: 'Новый делегат', brief: 'Проверь прокрутку.\nВторая строка.', harness: 'codex', model: 'gpt-6.1-sol', isolated: true });
    const spawn = card(page, fresh.call);
    await expect(spawn).toBeVisible();
    await expect(spawn).toHaveAttribute('data-grows', '');
    const animations = await spawn.evaluate((node) => node.getAnimations().map((item) => (item as CSSAnimation).animationName));
    if (motion === 'reduced') expect(animations).toEqual([]);
    await expect(spawn.locator('[data-role="card-state"]')).toHaveText('запускается');
    streamCall(hub, api, seed.id, seed.run, 'mcp__canterlot__session_spawn', null, { blockId: fresh.call, resultId: fresh.result, result: hubText({ session_id: seed.second, status: 'started', worktree: { branch: 'canterlot/0a1b2c3d', path: '/w' } }) });
    await expect(spawn.locator('[data-role="card-state"]')).toHaveText('завершена');
    await expect(feed(page)).toHaveAttribute('data-pinned', '');
    await expect.poll(() => atBottom(page)).toBeLessThanOrEqual(2);
    if (motion === 'reduced') {
      const live = await page.locator('.feed .dot[data-live]').evaluateAll((nodes) => nodes.flatMap((node) => node.getAnimations().map((item) => (item as CSSAnimation).animationName)));
      expect(live).toEqual([]);
    }
  });
}
