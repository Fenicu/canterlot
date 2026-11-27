import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { seedPlanWithLongTitles } from '../../testing/fixtures/plan';
import { bootSecondary } from './secondary-fixture';
import { expect, test } from './fixtures';

// Проверка геометрии в браузере: jsdom не считает переносы и размеры текста.
test.use({ reducedMotion: 'reduce' });

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SHARE_ID = '00000000-0000-4000-8000-0000000000e1';
const LONG_SESSION = 'Гейт на workstation: exec_e2e withdraw и cutover-signals TERM во время PWA и перенос длинного названия сессии';
const LONG_ROUTING_TYPE = 'Проверка длинного произвольного названия типа задач с переносом на узком экране и без обрезки текста';

/** Прямоугольник текста, а не контейнера: переполнение при nowrap видно только так. */
async function textRight(locator: import('@playwright/test').Locator): Promise<number> {
  return locator.evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    return range.getBoundingClientRect().right;
  });
}

test('длинные заголовки задач не раздвигают строку плана на телефоне', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedPlanWithLongTitles(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/plan/${seed.orchestrator}`);
  await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  for (const title of seed.longTitles) {
    await expect(page.locator('[data-plan-task] .title', { hasText: title })).toBeVisible();
  }

  const layout = await page.evaluate(() => {
    const body = document.querySelector('.secondary-body')!;
    return {
      scrollWidth: body.scrollWidth,
      clientWidth: body.clientWidth,
      rows: [...document.querySelectorAll('[data-plan-task]')].map((row) => {
        const title = row.querySelector('.title')!;
        const state = row.querySelector('.state')!;
        const range = document.createRange();
        range.selectNodeContents(title);
        return {
          number: row.getAttribute('data-plan-task'),
          title: title.textContent ?? '',
          textRight: range.getBoundingClientRect().right,
          rowRight: row.querySelector('.head')!.getBoundingClientRect().right,
          stateLeft: state.getBoundingClientRect().left
        };
      })
    };
  });

  expect(layout.scrollWidth, 'содержимое можно прокрутить вбок').toBeLessThanOrEqual(layout.clientWidth);
  for (const row of layout.rows) {
    expect(row.textRight, `заголовок задачи ${row.number} вышел за строку: ${row.title}`).toBeLessThanOrEqual(row.rowRight + 1);
    expect(row.stateLeft, `слово состояния наезжает на заголовок задачи ${row.number}`).toBeGreaterThanOrEqual(row.textRight - 1);
  }
  await expect(page.locator('[data-plan-group="done"] button.group-head .group-title')).toBeVisible();
});

test('длинное название сессии не раздвигает кнопку выбора на экране отправки', async ({ page }) => {
  const { ids } = await bootSecondary(page, '/machines', (api) => {
    seedConversation(api, { title: LONG_SESSION, last_activity_at: new Date().toISOString() });
  });
  // Хранилище хранит ключи с префиксом владельца.
  await page.evaluate(({ owner, id }) => new Promise<void>((resolve, reject) => {
    const request = indexedDB.open('canterlot', 1);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      const tx = db.transaction('share_inbox', 'readwrite');
      tx.objectStore('share_inbox').put({ id, receivedAt: Date.now(), files: [], title: 'Передача' }, `${owner}/${id}`);
      tx.oncomplete = () => { db.close(); resolve(); };
      tx.onerror = () => { db.close(); reject(tx.error); };
    };
  }), { owner: ids.pinned, id: SHARE_ID });
  await page.goto(`/share?id=${SHARE_ID}`);
  await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  const choice = page.locator('.choice').filter({ hasText: LONG_SESSION }).locator('button');
  await expect(choice).toBeVisible();
  const box = (await choice.boundingBox())!;
  expect(await textRight(choice.locator('strong')), 'название сессии вышло за кнопку').toBeLessThanOrEqual(box.x + box.width + 1);
});

test('длинное название типа задач не раздвигает кнопку удаления в маршрутизации', async ({ page }) => {
  await bootSecondary(page, '/settings/routing', (api) => {
    const policy = structuredClone(api.builtinRoutingPolicy) as { task_types: Record<string, { title: string }> };
    policy.task_types.long = { ...policy.task_types.bugfix!, title: LONG_ROUTING_TYPE };
    api.routingPolicy = policy;
  });
  await expect(page.locator('[data-routing-editor][data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  const remove = page.getByRole('button', { name: `Удалить тип «${LONG_ROUTING_TYPE}»` });
  await expect(remove).toBeVisible();
  const box = (await remove.boundingBox())!;
  expect(await textRight(remove), 'название типа вышло за кнопку').toBeLessThanOrEqual(box.x + box.width + 1);
  const body = await page.locator('.secondary-body').evaluate((element) => ({ scrollWidth: element.scrollWidth, clientWidth: element.clientWidth }));
  expect(body.scrollWidth, 'содержимое редактора можно прокрутить вбок').toBeLessThanOrEqual(body.clientWidth);
});
