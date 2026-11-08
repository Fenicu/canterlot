import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedPlanCards, type PlanCardsSeed } from '../../testing/fixtures/plan-cards';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c2';

async function boot(page: Page): Promise<{ hub: FakeHub; api: FakeHubApi; seed: PlanCardsSeed }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedPlanCards(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, seed };
}

const feed = (page: Page) => page.locator('.feed');
const card = (page: Page, id: string) => page.locator(`[data-key="block:${id}"] .feed-card`).first();

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

test('every_plan_card_renders_its_line_success_and_error', async ({ page }) => {
  test.setTimeout(90_000);
  const { seed } = await boot(page);
  const expected: [string, string, string, string][] = [
    ['add', 'canterlot.plan_add', 'План: добавлено 5 задач', ''],
    ['addError', 'canterlot.plan_add', 'План: задачи не добавлены', 'Неверный вызов: пустое название задачи'],
    ['spawnPlanError', 'canterlot.session_spawn', 'Не запустила «Миграция»', ''],
    ['updateStart', 'canterlot.plan_update', '#1 Таблица plan_tasks и миграция: ждёт → в работе', ''],
    ['updateBlocked', 'canterlot.plan_update', '#3 Перевод в in_review по подписке: ждёт → заблокирована', ''],
    ['updateReview', 'canterlot.plan_update', '#2 Хранилище плана и вкладка «План»: ждёт → на проверке', ''],
    ['updateError', 'canterlot.plan_update', 'План: задача #9 не обновлена', 'В плане нет задачи #9'],
    ['list', 'canterlot.plan_list', 'План: 2 в работе · 1 на проверке · 1 заблокирована · 1 ждёт', ''],
    ['listError', 'canterlot.plan_list', 'План: не прочитан', 'Неверный вызов: аргументы не подошли хабу'],
    ['adopt', 'canterlot.plan_adopt', 'Забрала план «Прошлый оркестратор»: 3 задачи', ''],
    ['adoptError', 'canterlot.plan_adopt', 'Не забрала план «Карточки плана»', 'Неверный вызов: план делегата забрать нельзя'],
    ['addFull', 'canterlot.plan_add', 'План: задачи не добавлены', 'План полон: в нём 200 задач, вмещается 200']
  ];
  for (const [name, id, line, error] of expected) {
    const found = await reveal(page, card(page, seed.blocks[name]!));
    await expect(found, name).toHaveAttribute('data-card', id);
    await expect(found.locator('.head .hit'), name).toHaveAttribute('aria-label', `${line}. Показать вызов`);
    if (error) {
      await expect(found.locator('[data-role="card-state"]'), name).toHaveText('ошибка');
      await expect(found.locator('[data-role="plan-error"]'), name).toHaveText(error);
    }
  }
  await expect((await reveal(page, card(page, seed.blocks.spawnPlanError!))).locator('[data-role="spawn-error"]')).toHaveText('Задача #1 уже готова: сначала её открывают заново');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test('plan_add_lists_numbered_titles_that_open_the_plan_tab', async ({ page }) => {
  const { seed } = await boot(page);
  const add = await reveal(page, card(page, seed.blocks.add!));
  const rows = add.locator('[data-role="plan-add-tasks"] [data-plan-task]');
  await expect(rows).toHaveCount(3);
  await expect(rows.first()).toContainText('#1 Таблица plan_tasks и миграция');
  // Задача ушла дальше: строка называет её состояние тогда и сейчас.
  await expect(rows.first().locator('[data-role="plan-row-state"]')).toHaveText(/ждёт\s*→\s*,? ?сейчас\s*готова/);
  await add.getByRole('button', { name: 'Все задачи · 5' }).click();
  await expect(rows).toHaveCount(5);
  await expect(add.getByRole('link', { name: '#5 Карточки плана в ленте' })).toHaveAttribute('href', `/plan/${seed.id}`);
  await add.getByRole('link', { name: '#4 Документация API' }).click();
  const tab = page.locator('[data-role="plan-tab"]');
  await expect(tab).toBeVisible();
  await expect(page.getByRole('radio', { name: 'План' })).toBeChecked();
  await expect(tab.locator('[data-plan-task="4"]')).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`/c/${seed.id}$`));
});

test('plan_update_shows_the_transition_details_and_the_live_state', async ({ page }) => {
  const { api, seed } = await boot(page);
  const start = await reveal(page, card(page, seed.blocks.updateStart!));
  // После правки задачу закрыли: справа её состояние сейчас.
  await expect(start.locator('[data-role="card-state"]')).toHaveText('сейчас готова');
  await expect(start.locator('[data-role="card-state"]')).toHaveAttribute('data-tone', 'success');
  await expect(start.locator('[data-role="plan-update-move"] [data-state="in_progress"]')).toHaveAttribute('data-tone', 'accent');

  const blocked = await reveal(page, card(page, seed.blocks.updateBlocked!));
  await expect(blocked.locator('[data-role="plan-update-blocker"]')).toHaveText('Блокер: Нужен общий ответ гейта толчка, иначе толчок уйдёт дважды');
  await expect(blocked.locator('[data-role="card-state"]')).toHaveCount(0);

  const review = await reveal(page, card(page, seed.blocks.updateReview!));
  await expect(review.locator('[data-role="plan-update-findings"] li')).toHaveCount(2);
  await expect(review.locator('[data-role="card-state"]')).toHaveCount(0);
  // Событие хаба принимает задачу: карточка правки узнаёт об этом на месте.
  api.pushPlanTask({ ...seed.tasks[2]!, state: 'done' });
  await expect(review.locator('[data-role="card-state"]')).toHaveText('сейчас готова');
});

test('plan_list_expands_to_the_tasks_in_screen_order', async ({ page }) => {
  const { seed } = await boot(page);
  const list = await reveal(page, card(page, seed.blocks.list!));
  await expect(list.locator('[data-role="plan-list-tasks"]')).toHaveCount(0);
  await list.getByRole('button', { name: 'Весь план · 5' }).click();
  const rows = list.locator('[data-role="plan-list-tasks"] [data-plan-task]');
  await expect(rows).toHaveCount(5);
  expect(await rows.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-plan-task')))).toEqual(['1', '5', '2', '3', '4']);
  await expect(list.locator('.head a')).toHaveText('План');
  await expect(list.locator('.head a')).toHaveAttribute('href', `/plan/${seed.id}`);
});

test('plan_adopt_maps_old_numbers_to_new_ones', async ({ page }) => {
  const { seed } = await boot(page);
  const adopt = await reveal(page, card(page, seed.blocks.adopt!));
  await expect(adopt.locator('.head a')).toHaveAttribute('href', `/c/${seed.old}`);
  const rows = adopt.locator('[data-role="plan-adopt-moved"] [data-plan-task]');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toContainText('#2 →');
  await expect(rows.nth(0)).toContainText('#6 Сторож лимитов на машине');
  await expect(rows.nth(0).locator('[data-role="plan-row-state"]')).toHaveText('в работе');
});

test('spawn_card_names_its_plan_task_and_follows_it', async ({ page }) => {
  const { api, seed } = await boot(page);
  const spawn = await reveal(page, card(page, seed.blocks.spawnPlan!));
  const line = spawn.locator('[data-role="spawn-plan-task"]');
  await expect(line).toContainText('задача');
  await expect(line.getByRole('link', { name: '#5 Карточки плана в ленте' })).toHaveAttribute('href', `/plan/${seed.id}`);
  await expect(line.locator('[data-state]')).toHaveText('в работе');
  api.pushPlanTask({ ...seed.tasks[5]!, state: 'in_review' });
  await expect(line.locator('[data-state]')).toHaveText('на проверке');
  await expect(line.locator('[data-state]')).toHaveAttribute('data-tone', 'blue');
});
