import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
// Путь без пробелов, точек и дефисов: браузер не рвёт его сам ни по «/», ни по «.».
const LONG_PATH = '/home/fenicu/myprojects/canterlot/web/src/lib/timeline/PendingStrip.svelte';
const LONG_TOKEN = 'sha256:9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0';

const WIDTHS = [
  { width: 360, height: 740 },
  { width: 390, height: 844 },
  { width: 412, height: 892 }
] as const;

async function boot(
  page: Page,
  opts: { theme?: 'dark' | 'light'; viewport?: { width: number; height: number } } = {}
) {
  if (opts.theme) {
    await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), opts.theme);
  }
  if (opts.viewport) await page.setViewportSize(opts.viewport);

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
    title: 'Карточки запросов',
    model: 'opus-5.5',
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

function openRequest(hub: FakeHub, api: FakeHubApi, id: string, kind: string, payload: unknown) {
  hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    payload: { request_id: api.id(), kind, payload }
  });
  hub.flush();
}

const LONG_OPTIONS = Array.from({ length: 8 }, (_, index) => ({
  label: `Вариант ${index + 1}`,
  description: 'Описание варианта в одну-две строки, чтобы карточка выросла выше слота'
}));

function openPermissionAndQuestion(hub: FakeHub, api: FakeHubApi, id: string) {
  const command = Array.from({ length: 40 }, (_, index) => `cargo test -p canterlot-hub step_${index} -- --nocapture`).join(' && \\\n');
  openRequest(hub, api, id, 'permission', {
    tool_name: 'Bash',
    description: 'Запустить тесты хаба по шагам',
    command
  });
  openRequest(hub, api, id, 'question', {
    questions: [
      {
        id: 'q0',
        text: 'Какой вариант миграции выбрать для таблицы лимитов?',
        options: LONG_OPTIONS
      }
    ]
  });
}

/** Чернила текста: объединение прямоугольников строк, а не box элемента. */
async function ink(locator: Locator) {
  return locator.evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    const boxes = [...range.getClientRects()].filter((rect) => rect.width > 0 && rect.height > 0);
    return {
      lines: boxes.length,
      ...boxes.reduce(
        (acc, rect) => ({
          left: Math.min(acc.left, rect.left),
          right: Math.max(acc.right, rect.right),
          top: Math.min(acc.top, rect.top),
          bottom: Math.max(acc.bottom, rect.bottom)
        }),
        { left: Number.POSITIVE_INFINITY, right: Number.NEGATIVE_INFINITY, top: Number.POSITIVE_INFINITY, bottom: Number.NEGATIVE_INFINITY }
      )
    };
  });
}

/** Карточка не выпускает текст за поля и не становится боковым скроллером. */
async function expectTextInsideCard(page: Page, textSelector: string) {
  const card = page.locator('.request-cards-slot > .card-root').first();
  await expect(card).toBeVisible();
  const geometry = await card.evaluate((element, selector) => {
    const target = element.querySelector(selector);
    if (!target) return null;
    const box = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    return {
      contentLeft: box.left + parseFloat(style.paddingLeft),
      contentRight: box.right - parseFloat(style.paddingRight),
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
      overflowX: style.overflowX
    };
  }, textSelector);
  expect(geometry).not.toBeNull();
  const inkBox = await ink(page.locator(`.request-cards-slot > .card-root ${textSelector}`).first());
  expect(inkBox.lines).toBeGreaterThan(0);

  // 1 px — допуск на выступание глифа за свою строку.
  expect(inkBox.left).toBeGreaterThanOrEqual(geometry!.contentLeft - 1);
  expect(inkBox.right).toBeLessThanOrEqual(geometry!.contentRight + 1);
  // Боковой прокрутки нет ни у карточки, ни у всего документа.
  expect(geometry!.scrollWidth).toBeLessThanOrEqual(geometry!.clientWidth + 1);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
    await page.evaluate(() => document.documentElement.clientWidth + 1)
  );
  expect(['clip', 'hidden']).toContain(geometry!.overflowX);
}

for (const { width, height } of WIDTHS) {
  for (const theme of ['dark', 'light'] as const) {
    test(`permission description with an unbroken path wraps inside the card at ${width} ${theme}`, async ({ page }) => {
      const { hub, api, id } = await boot(page, { theme, viewport: { width, height } });
      openRequest(hub, api, id, 'permission', {
        tool_name: 'Bash',
        description: `Проверить типы в ${LONG_PATH}`,
        command: `cd ${LONG_PATH.replace('/web/src/lib/timeline/PendingStrip.svelte', '')} && npm run check`,
        cwd: LONG_PATH
      });
      await expect(page.locator('.permission-card')).toBeVisible();
      await expect(page.locator('.description')).toHaveText(`Проверить типы в ${LONG_PATH}`);
      await expectTextInsideCard(page, '.description');
    });

    test(`question title and header with an unbroken path wrap inside the card at ${width} ${theme}`, async ({ page }) => {
      const { hub, api, id } = await boot(page, { theme, viewport: { width, height } });
      openRequest(hub, api, id, 'question', {
        questions: [
          {
            id: 'q0',
            header: `МИГРАЦИЯ ${LONG_TOKEN}`,
            text: `Слить ${LONG_PATH} в основную ветку и переписать импорты?`,
            options: [{ label: 'Да, слить' }, { label: 'Нет, сначала ревью' }]
          }
        ]
      });
      await expect(page.locator('.question-card')).toBeVisible();
      await expectTextInsideCard(page, '.q-text');
      await expectTextInsideCard(page, '.q-header');
    });
  }
}

test('codex reason with an unbroken path wraps inside the permission card', async ({ page }) => {
  const { hub, api, id } = await boot(page, { theme: 'dark', viewport: { width: 412, height: 892 } });
  openRequest(hub, api, id, 'permission', {
    tool_name: 'apply_patch',
    reason: `Нужно поправить ${LONG_PATH}, иначе заголовок карточки налезает на значок при длинном названии сессии`,
    cwd: LONG_PATH
  });
  await expect(page.locator('.permission-card')).toBeVisible();
  await expect(page.locator('.description')).toHaveText(/^Нужно поправить \/home/);
  await expectTextInsideCard(page, '.description');
});

test('mcp elicitation message with an unbroken token wraps inside the permission card', async ({ page }) => {
  const { hub, api, id } = await boot(page, { theme: 'light', viewport: { width: 390, height: 844 } });
  openRequest(hub, api, id, 'permission', {
    tool_name: 'mcp_elicitation',
    server: 'plugin_taiga_taiga-with-long-server-name',
    message: `Подтвердите доступ к проекту ${LONG_PATH} по токену ${LONG_TOKEN}`
  });
  await expect(page.locator('.permission-card')).toBeVisible();
  await expect(page.locator('.description')).toHaveText(/^Подтвердите доступ/);
  await expectTextInsideCard(page, '.description');
});
