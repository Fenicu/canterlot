import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedTaigaCards, type TaigaCardsSeed } from '../../testing/fixtures/taiga-cards';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c1';

async function boot(page: Page): Promise<{ hub: FakeHub; api: FakeHubApi; seed: TaigaCardsSeed }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedTaigaCards(api);
  await installChatRoutes(page, hub, api, OWNER);
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
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
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

test('every_taiga_card_kind_renders_with_its_rich_content', async ({ page }) => {
  test.setTimeout(180_000);
  const { seed } = await boot(page);
  const expected: [string, string, string][] = [
    ['search', 'taiga.search', 'Taiga · поиск «склад»: 7'],
    ['get', 'taiga.userstory_get', 'Taiga · открыла историю #123 «Фильтр товаров по складу»'],
    ['comments', 'taiga.comment_list', 'Taiga · комментарии к истории #123: 3'],
    ['comment', 'taiga.comment_add', 'Taiga · комментарий к истории #123'],
    ['update', 'taiga.userstory_update', 'Taiga · обновила историю #123 «Фильтр товаров по складу»'],
    ['create', 'taiga.task_create', 'Taiga · создала задачу #207 «Проверить выгрузку остатков»'],
    ['bulk', 'taiga.bulk_create', 'Taiga · создала 6 задач'],
    ['link', 'taiga.link', 'Taiga · связала задачу #201 с историей #123'],
    ['list', 'taiga.userstory_list', 'Taiga · истории спринта «Спринт 8»: 12'],
    ['sprints', 'taiga.sprint_list', 'Taiga · спринты: 3'],
    ['wikiGet', 'taiga.wiki_get', 'Taiga · открыла страницу вики «home»'],
    ['wikiCreate', 'taiga.wiki_create', 'Taiga · создала страницу вики «release-notes»'],
    ['wikiUpdate', 'taiga.wiki_update', 'Taiga · обновила страницу вики «release-notes»'],
    ['delete', 'taiga.task_delete', 'Taiga · удалила задачу #198'],
    ['running', 'taiga.userstory_get', 'Taiga · открыла историю #124'],
    ['error', 'taiga.userstory_update', 'Taiga · обновила историю #123']
  ];
  for (const [name, id, text] of expected) {
    const found = await reveal(page, card(page, seed.blocks[name]!));
    await expect(found, name).toHaveAttribute('data-card', id);
    await expect(found.locator('.head .lead'), name).toHaveText(text);
  }

  // Заголовок ведёт в Taiga: история, задача и страница вики.
  await expect((await reveal(page, card(page, seed.blocks.get!))).locator('.head a')).toHaveAttribute('href', 'https://taiga.example.com/project/sklad/us/123');
  await expect((await reveal(page, card(page, seed.blocks.create!))).locator('.head a')).toHaveAttribute('href', 'https://taiga.example.com/project/sklad/task/201');
  await expect((await reveal(page, card(page, seed.blocks.wikiGet!))).locator('.head a')).toHaveAttribute('href', 'https://taiga.example.com/project/sklad/wiki/home');

  // Открытая история: поля ответа и красное слово блокировки.
  const one = await reveal(page, card(page, seed.blocks.get!));
  await expect(one.locator('[data-role="taiga-meta"]')).toHaveText('В работе · Анна Смирнова · Спринт 8 · 5 очков · заблокирована');
  await expect(one.locator('[data-role="taiga-blocked"]')).toHaveCSS('color', /rgb\(/);

  // Правка: владелец читает, что именно изменилось.
  const update = await reveal(page, card(page, seed.blocks.update!));
  await expect(update.locator('[data-role="taiga-changes"]')).toHaveText(/статус → Готово к тестированию.*исполнитель → Илья Котов.*тема → «Фильтр товаров по складу».*добавила теги: срочно.*разблокировала/s);

  // Комментарий: свёрнут до шести строк, раскрывается целиком.
  const comment = await reveal(page, card(page, seed.blocks.comment!));
  await expect(comment.locator('[data-role="taiga-comment"]')).toHaveClass(/clamped/);
  await comment.locator('[data-role="taiga-comment-toggle"]').click();
  await expect(comment.locator('[data-role="taiga-comment"]')).not.toHaveClass(/clamped/);
  await expect(comment.locator('[data-role="taiga-comment"]')).toContainText('Проверка на 320 px прошла');
  await expect((await reveal(page, card(page, seed.blocks.comments!))).locator('[data-role="taiga-last-comment"]')).toContainText('Осталось проверить сброс фильтра');

  // Список: пять строк и «ещё 7», после раскрытия — весь ответ.
  const list = await reveal(page, card(page, seed.blocks.list!));
  await expect(list.locator('[data-role="taiga-rows"] .row')).toHaveCount(5);
  await list.locator('[data-role="taiga-more"]').click();
  await expect(list.locator('[data-role="taiga-rows"] .row')).toHaveCount(12);
  await expect(list.locator('[data-role="taiga-rows"] .row').first()).toContainText('#123 Фильтр товаров по складу · Готово к тестированию');

  // Пачка: пять созданных, «ещё 1».
  const bulk = await reveal(page, card(page, seed.blocks.bulk!));
  await expect(bulk.locator('[data-role="taiga-created"] .row')).toHaveCount(5);
  await bulk.locator('[data-role="taiga-more"]').click();
  await expect(bulk.locator('[data-role="taiga-created"] .row')).toHaveCount(6);
  await expect(bulk.locator('[data-role="taiga-created"] .row').last()).toContainText('#206 Написать тесты выгрузки');

  // Поиск: строки из разных разделов и «ещё 2».
  const search = await reveal(page, card(page, seed.blocks.search!));
  await expect(search.locator('[data-role="taiga-rows"] .row')).toHaveCount(5);
  await search.locator('[data-role="taiga-more"]').click();
  await expect(search.locator('[data-role="taiga-rows"] .row')).toHaveCount(7);
  await expect(search.locator('[data-role="taiga-rows"] .kind')).toHaveText(['история', 'история', 'история', 'задача', 'задача', 'задача', 'вики']);

  // Идёт и ошибка: слово состояния и текст ошибки.
  await expect((await reveal(page, card(page, seed.blocks.running!))).locator('[data-role="card-state"]')).toHaveText('идёт');
  const error = await reveal(page, card(page, seed.blocks.error!));
  await expect(error.locator('[data-role="card-state"]')).toHaveText('ошибка');
  await expect(error.locator('[data-role="taiga-error"]')).toHaveText('"Спринт 9" is not a sprint in this project.');

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test('taiga_service_calls_fold_into_a_quiet_row', async ({ page }) => {
  const { seed } = await boot(page);
  const group = await reveal(page, page.locator('[data-role="quiet-group"]').first());
  await expect(group.locator('.line')).toHaveText('Служебные вызовы · 3 · Профиль Taiga, Схема проекта, Статистика Taiga');
  await expect(row(page, seed.blocks.whoami!)).toHaveCount(0);
  await group.locator('.head').click();
  await expect(card(page, seed.blocks.whoami!).locator('.lead')).toHaveText('Профиль Taiga');
  await expect(card(page, seed.blocks.schema!).locator('.lead')).toHaveText('Схема проекта');
  await expect(card(page, seed.blocks.stats!).locator('.lead')).toHaveText('Статистика Taiga: Спринт 8');
  await expect(card(page, seed.blocks.whoami!)).toHaveAttribute('data-quiet', '');
});
