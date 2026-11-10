import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { seedSystemBubbles, type SystemBubbleGroup } from '../../testing/fixtures/system-bubbles';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, group: SystemBubbleGroup = 'all') {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 412 : 1280, height: 900 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Оркестратор' });
  const seed = seedSystemBubbles(api, id, group);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id, ...seed };
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
}

test('alarm_bubbles_carry_the_condition_note_targets_and_the_folded_text', async ({ page }) => {
  const { child } = await boot(page, 'alarm');
  const single = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'Будильник «gate»' }).first();
  await single.scrollIntoViewIfNeeded();
  await expect(single).toContainText('Пора');
  await expect(single).toContainText('Поставлен');
  await expect(single).toContainText('ждал');
  await expect(single.getByRole('button', { name: 'Показать заметку' })).toBeVisible();
  await single.getByRole('button', { name: 'Показать заметку' }).click();
  await expect(single.getByRole('button', { name: 'Скрыть заметку' })).toBeVisible();
  const fold = single.getByRole('button', { name: 'Текст для агента' });
  await expect(fold).toHaveAttribute('aria-expanded', 'false');
  await expect(single).not.toContainText('[canterlot]');
  await fold.click();
  await expect(fold).toHaveAttribute('aria-expanded', 'true');
  await expect(single.locator('[data-role="system-body"]')).toContainText('[canterlot] Alarm');

  const file = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'release' });
  await expect(file).toContainText('Появился файл /tmp/gate.rc (2 КиБ)');

  const timeout = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'watch' });
  await expect(timeout).toHaveAttribute('data-tone', 'warning');
  await expect(timeout).toContainText('Истёк срок ожидания');
  const link = timeout.getByRole('link', { name: 'Сборка' });
  await expect(link).toHaveAttribute('href', `/c/${child}`);
  await expect(timeout).toContainText('зависла');

  const batch = page.locator('article.system[data-event-kind="alarm"]').filter({ hasText: 'Сработало 2 будильника' });
  await expect(batch).toContainText('Будильник «gate»');
  await expect(batch).toContainText('Будильник «disk»');

  // Старый будильник без новых полей остаётся прежней тонкой строкой.
  await expect(page.locator('[data-role="alarm"]')).toContainText('Будильник «old»: сработал');
  await noOverflow(page);
});

test('machine_bubbles_show_numbers_and_open_the_machines_screen', async ({ page }) => {
  await boot(page, 'machine');
  const disk = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'Машина dev-vm: мало места' });
  await disk.scrollIntoViewIfNeeded();
  await expect(disk).toContainText('21,4 ГиБ (8 %)');
  await expect(disk).toContainText('/w/target');
  await expect(disk).toContainText('Баннер связи');

  const critical = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'критически мало места' });
  await expect(critical).toHaveAttribute('data-tone', 'warning');

  const memory = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'мало памяти' });
  await expect(memory).toHaveAttribute('data-tone', 'warning');
  await expect(memory).toContainText('2,9 ГиБ из 29,8 ГиБ');
  await expect(memory).toContainText('Toolkit');

  const leaked = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'остались процессы' });
  await expect(leaked).toContainText('cargo');
  const lease = page.locator('article.system[data-event-kind="machine"]').filter({ hasText: 'аренда занята долго' });
  await expect(lease).toContainText('95 мин');

  // Старое предупреждение без details падает на прежний текстовый блок.
  await expect(page.locator('[data-event-kind="legacy"]')).toContainText('Сообщение хаба');

  await disk.getByRole('link', { name: 'Открыть машины' }).click();
  await expect(page).toHaveURL(/\/machines$/);
});

test('context_bubbles_offer_the_same_compaction_request_in_the_own_session', async ({ page }) => {
  await boot(page, 'context');
  const first = page.locator('article.system[data-event-kind="context"]').filter({ hasText: '85 %' });
  await first.scrollIntoViewIfNeeded();
  await expect(first).toContainText('Контекст заполнен на 85 %');
  await expect(first).toHaveAttribute('data-tone', 'neutral');
  const second = page.locator('article.system[data-event-kind="context"]').filter({ hasText: '92 %' });
  await expect(second).toHaveAttribute('data-tone', 'warning');
  const foreign = page.locator('article.system[data-event-kind="context"]').filter({ hasText: '88 %' });
  await expect(foreign.getByRole('button', { name: 'Сжать контекст' })).toHaveCount(0);

  let posts = 0;
  await page.route('**/api/v1/conversations/*/compact', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    posts += 1;
    await route.fulfill({
      status: 200,
      headers: { 'content-type': 'application/json' },
      json: { requested: { command_id: '00000000-0000-4000-8000-000000000099' } }
    });
  });
  await first.getByRole('button', { name: 'Сжать контекст' }).click();
  await expect.poll(() => posts).toBe(1);
});

test('limit_bubbles_name_the_provider_window_reset_and_the_stuck_delegate', async ({ page }) => {
  const { child } = await boot(page, 'limit');
  const resumed = page.locator('article.system[data-event-kind="limit_resumed"]');
  await resumed.scrollIntoViewIfNeeded();
  await expect(resumed).toContainText('Лимит Claude сброшен — продолжаю');
  await expect(resumed).toContainText('неделя');

  const paused = page.locator('article.system[data-event-kind="quota_paused"]');
  await expect(paused).toContainText('Помощник «Сборка» упёрся в лимит Claude (неделя)');
  await expect(paused).toContainText('сброс');
  await expect(paused.getByRole('link', { name: /Помощник «Сборка»/ })).toHaveAttribute('href', `/c/${child}`);

  const note = page.locator('article.system[data-event-kind="quota_note"]');
  await expect(note).toContainText('Лимит сброшен, но «Сборка» сам не продолжит');
  await expect(note).toContainText('Отправьте ему сообщение или остановите');
  await expect(note.locator('.action')).toHaveCount(0);
  await expect(note.getByRole('button', { name: 'Отправьте ему сообщение или остановите' })).toHaveCount(0);
  await expect(note.getByRole('link', { name: /Лимит сброшен/ })).toHaveAttribute('href', `/c/${child}`);
});

test('a_session_message_is_an_agent_bubble_with_a_bookmark_and_no_edit', async ({ page }) => {
  const { child } = await boot(page, 'message');
  const message = page.locator('[data-role="session-message"]').first();
  await message.scrollIntoViewIfNeeded();
  await expect(message).toContainText('Сессия «Сборка»');
  await expect(message).toContainText('Sonnet 5');
  await expect(message).not.toContainText('[canterlot]');
  await expect(message).toContainText('Проверь миграцию');
  await expect(message.getByRole('link', { name: 'Сессия «Сборка»' })).toHaveAttribute('href', `/c/${child}`);
  await expect(message.getByRole('button', { name: 'Изменить' })).toHaveCount(0);
  const bookmark = message.getByRole('button', { name: 'В избранное' });
  await expect(bookmark).toHaveAttribute('aria-pressed', 'false');
  const headerBox = (await message.locator('header').boundingBox())!;
  const toggleBox = (await bookmark.boundingBox())!;
  expect(toggleBox.y + toggleBox.height / 2).toBeGreaterThanOrEqual(headerBox.y);
  expect(toggleBox.y + toggleBox.height / 2).toBeLessThanOrEqual(headerBox.y + headerBox.height);
  await bookmark.click();
  await expect(message.getByRole('button', { name: 'Убрать из избранного' })).toHaveAttribute('aria-pressed', 'true');

  // Владелец и дальше видит свой пузырь с правкой, чужое сообщение — нет.
  await expect(page.locator('article.bubble').filter({ hasText: 'Моё сообщение' })).toBeVisible();
});

test('a_spawn_brief_shows_twelve_lines_then_expands', async ({ page }) => {
  await boot(page, 'message');
  const brief = page.locator('[data-role="session-message"][data-origin="spawn"]');
  await brief.scrollIntoViewIfNeeded();
  await expect(brief).toContainText('Задание от сессии «Оркестратор»');
  await expect(brief).toContainText('Шаг 12');
  await expect(brief).not.toContainText('Шаг 13');
  await brief.getByRole('button', { name: 'Показать полностью' }).click();
  await expect(brief).toContainText('Шаг 18');
  await brief.getByRole('button', { name: 'Свернуть' }).click();
  await expect(brief).not.toContainText('Шаг 13');
});

test('the_service_bubbles_do_not_overflow_320_and_412_and_have_distinct_names', async ({ page }) => {
  await boot(page, 'all');
  for (const width of [320, 412]) {
    await page.setViewportSize({ width, height: 900 });
    await noOverflow(page);
    for (const bubble of await page.locator('article.system').all()) {
      const names = await bubble.locator('a,button').evaluateAll((nodes) =>
        nodes.map((node) => (node.getAttribute('aria-label') || node.textContent || '').trim())
      );
      for (const name of names) expect(name, `пустое имя в ${width}px`).not.toBe('');
      expect(new Set(names).size, `повтор имени в ${width}px: ${names.join(', ')}`).toBe(names.length);
    }
  }
});
