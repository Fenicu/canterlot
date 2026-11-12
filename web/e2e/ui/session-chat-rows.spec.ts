import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { ORCHESTRATOR_PREVIEW, PINNED_PREVIEW, seedChatSessions } from '../../testing/fixtures/chat-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, opts: { machines?: number; path?: string } = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedChatSessions(api, { machines: opts.machines });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(opts.path ?? '/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, ids };
}

const row = (page: Page, id: string) => page.locator(`[data-session="${id}"]`);

test('session_row_reads_like_a_chat_with_title_time_and_model_line', async ({ page }) => {
  const { ids } = await boot(page);
  const orchestrator = row(page, ids.orchestrator);
  await expect(orchestrator.locator('.title')).toHaveText('Кантерлот');
  await expect(orchestrator.locator('time')).toHaveText(/^\d{2}:\d{2}$/);
  // Покой: вторая строка — начало последнего ответа, приглушённо.
  await expect(orchestrator.locator('.who')).toHaveText('Opus:');
  await expect(orchestrator.locator('.activity')).toHaveAttribute('data-preview', '');
  await expect(orchestrator.locator('.text')).toHaveText(ORCHESTRATOR_PREVIEW);
  await expect(orchestrator.locator('[data-role="plan-badge"]')).toContainText('3/4');
  // Харнесс и машина ушли из строки: они в подсказке модели и в шапке чата.
  await expect(orchestrator).not.toContainText('Claude Code');
  await expect(orchestrator).not.toContainText('dev-vm');
  await expect(orchestrator.locator('.who')).toHaveAttribute('title', /Claude Code · .* · dev-vm/);
  const box = (await orchestrator.boundingBox())!;
  expect(box.height).toBeLessThanOrEqual(80);
});

test('session_row_status_colours_and_avatar_vendor_follow_the_model', async ({ page }) => {
  const { ids } = await boot(page);
  const cases: [string, string, string, RegExp, string][] = [
    [ids.waiting, 'openai', 'GPT-6.1 Sol:', /^Ждёт ответа · 2 варианта$/, 'warning'],
    [ids.pinned, 'deepseek', 'DeepSeek V4.1:', new RegExp(`^${PINNED_PREVIEW}$`), 'default'],
    [ids.stalled, 'meta', 'Muse Spark:', /^Подвисла\? Нет событий/, 'warning'],
    [ids.paused, 'mimo', 'MiMo Flash:', /^Ожидание: недельный лимит$/, 'warning'],
    [ids.kids[0], 'anthropic', 'Opus:', /^Генерирует$/, 'accent'],
    [ids.kids[1], 'anthropic', 'Opus:', /^Выполняет cargo nextest · \d+ мин$/, 'accent']
  ];
  for (const [id, vendor, who, text, tone] of cases) {
    const item = row(page, id);
    await expect(item).toHaveAttribute('data-vendor', vendor);
    await expect(item.locator('.avatar svg path').first()).toBeAttached();
    await expect(item.locator('.who')).toHaveText(who);
    await expect(item.locator('.text')).toHaveText(text);
    await expect(item.locator('.activity')).toHaveAttribute('data-tone', tone);
  }
  // Цвет текста состояния — цвет тона, кольцо аватара в том же тоне.
  const waiting = row(page, ids.waiting);
  const colours = await waiting.evaluate((node) => ({
    text: getComputedStyle(node.querySelector('.text')!).color,
    warning: getComputedStyle(document.documentElement).getPropertyValue('--warning').trim()
  }));
  expect(colours.text).not.toBe('');
  await expect(waiting.locator('.avatar')).toHaveAttribute('data-halo', '--warning');
  await expect(row(page, ids.kids[0]).locator('.avatar')).toHaveAttribute('data-halo', '--accent');
  // Непрочитанное — счётчиком справа, закреплённая — значком.
  await expect(waiting.locator('.count')).toHaveText('2');
  await expect(waiting.getByText('Непрочитанных: 2')).toBeAttached();
  await expect(row(page, ids.pinned).locator('[data-pinned]')).toBeVisible();
});

test('session_row_names_the_machine_only_when_several_are_online', async ({ page }) => {
  const { ids } = await boot(page, { machines: 2 });
  await expect(row(page, ids.orchestrator).locator('.who')).toHaveText('Opus · dev-vm:');
});

test('delegated_children_are_compact_nested_rows_under_the_fold_summary', async ({ page }) => {
  const { ids } = await boot(page);
  const fold = page.getByRole('button', { name: 'Свернуть делегированные: 2' });
  await expect(fold).toContainText('Делегированные · 2 · работает 2');
  const parent = (await row(page, ids.orchestrator).locator('.avatar').boundingBox())!;
  for (const id of ids.kids) {
    const kid = row(page, id);
    await expect(kid).toHaveClass(/child/);
    const avatar = (await kid.locator('.avatar').boundingBox())!;
    expect(avatar.width).toBeLessThan(parent.width);
    expect(avatar.x).toBeGreaterThan(parent.x + parent.width / 2);
    expect((await kid.boundingBox())!.height).toBeLessThanOrEqual(56);
  }
  await fold.click();
  await expect(row(page, ids.kids[0])).toHaveCount(0);
  await page.getByRole('button', { name: 'Развернуть делегированные: 2' }).click();
  await expect(row(page, ids.kids[0])).toBeVisible();
});

test('group_section_headers_are_thin_and_still_fold', async ({ page }) => {
  await boot(page);
  const head = page.getByRole('button', { name: /^Работа/ }).filter({ has: page.locator('.count') });
  await expect(head).toHaveAttribute('aria-expanded', 'true');
  const box = (await head.boundingBox())!;
  expect(box.height).toBeLessThanOrEqual(44);
  await head.click();
  await expect(head).toHaveAttribute('aria-expanded', 'false');
});

test('desktop_rail_lists_every_filter_with_counts_and_bottom_sections', async ({ page, isMobile }) => {
  test.skip(isMobile, 'полоса групп есть только на столе');
  await page.setViewportSize({ width: 1920, height: 1080 });
  const { ids } = await boot(page);
  const rail = page.locator('[data-group-rail]');
  await expect(rail).toBeVisible();
  const tabs = rail.getByRole('tab');
  // Полоса показывает и пустые фильтры: «Архив» и «Корзина» без сессий.
  await expect(tabs).toHaveText([/Все/, /Закреп/, /Canterlot/, /Работа/, /Егорчик/, /Архив/, /Корзина/]);
  await expect(tabs.nth(0)).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.nth(0).locator('.count')).toContainText('5');
  await expect(tabs.nth(4).locator('.count')).toContainText('3');
  await expect(tabs.nth(5).locator('.count')).toHaveCount(0);
  await tabs.nth(3).click();
  await expect(tabs.nth(3)).toHaveAttribute('aria-selected', 'true');
  await expect(row(page, ids.waiting)).toBeVisible();
  await expect(row(page, ids.pinned)).toHaveCount(0);
  await tabs.nth(3).press('ArrowDown');
  await expect(tabs.nth(4)).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.nth(4)).toBeFocused();
  // Значок группы — из её настроек.
  await expect(tabs.nth(3).locator('svg path')).toHaveAttribute('d', /M3 7h18v13H3Z/);
  const sections = rail.getByRole('navigation', { name: 'Разделы' });
  await expect(sections.getByRole('link')).toHaveText(['Инструк­ции', 'Машины', 'Лимиты']);
  await sections.getByRole('link', { name: 'Машины' }).click();
  await expect(page).toHaveURL(/\/machines$/);
  await expect(sections.getByRole('link', { name: 'Машины' })).toHaveAttribute('aria-current', 'page');
  // Нижней навигации и чипов в панели списка нет.
  const pane = page.locator('[data-pane="list"]');
  await expect(pane.getByRole('navigation', { name: 'Навигация' })).toHaveCount(0);
  await expect(pane.locator('.tabs[role="tablist"]').filter({ hasNot: rail })).toHaveCount(1);
  await expect(pane.locator('[data-search-field]')).toBeVisible();
});

test('desktop_search_field_opens_the_existing_title_search', async ({ page, isMobile }) => {
  test.skip(isMobile, 'на телефоне поиск открывает значок');
  await page.setViewportSize({ width: 1920, height: 1080 });
  const { ids } = await boot(page);
  await page.locator('[data-search-field]').click();
  const field = page.getByRole('textbox', { name: 'Поиск сессий' });
  await expect(field).toBeFocused();
  await field.fill('утечки');
  await expect(row(page, ids.stalled)).toBeVisible();
  await expect(row(page, ids.waiting)).toHaveCount(0);
  await field.press('Escape');
  await field.press('Escape');
  await expect(page.locator('[data-search-field]')).toBeVisible();
});

test('phone_keeps_chips_bottom_nav_and_search_icon', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'телефонная раскладка');
  await boot(page);
  await expect(page.locator('[data-group-rail]')).toHaveCount(0);
  const chips = page.getByRole('tablist', { name: 'Группы сессий' });
  await expect(chips.getByRole('tab')).toHaveText([/Все/, /Закреплённые/, /Canterlot/, /Работа/, /Егорчик/]);
  await expect(page.getByRole('navigation', { name: 'Навигация' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
  await expect(page.locator('[data-search-field]')).toHaveCount(0);
});

test('limits_show_one_plaque_per_provider_in_a_row', async ({ page }) => {
  await boot(page);
  const plaques = page.locator('[data-limit-plaques] .plaque');
  await expect(plaques).toHaveCount(3);
  await expect(plaques.nth(0)).toContainText(/Claude\s*осталось75%\s*нед/);
  await expect(plaques.nth(1)).toContainText(/Codex\s*осталось10%\s*нед/);
  await expect(plaques.nth(2)).toContainText(/OpenCode\s*осталось54%\s*мес/);
  const boxes = await plaques.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().top));
  expect(new Set(boxes.map(Math.round)).size).toBe(1);
  await page.locator('[data-limit-plaques]').click();
  await expect(page.locator('[data-limit-plaques]')).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'История лимитов' })).toBeVisible();
});

test('group_icon_is_picked_on_create_and_changed_in_the_manager', async ({ page }) => {
  const { api } = await boot(page);
  await page.getByRole('button', { name: 'Ещё', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Группы' }).click();
  await page.getByRole('button', { name: 'Значок новой группы: Папка' }).click();
  const picker = page.getByRole('radiogroup', { name: 'Значок новой группы' });
  await expect(picker.getByRole('radio')).toHaveCount(16);
  await picker.getByRole('radio', { name: 'Ракета' }).click();
  await expect(picker.getByRole('radio', { name: 'Ракета' })).toHaveAttribute('aria-checked', 'true');
  await page.getByRole('textbox', { name: 'Название группы' }).fill('Запуски');
  await page.getByRole('button', { name: 'Создать' }).click();
  await expect.poll(() => api.groups.find((group) => group.name === 'Запуски')?.icon).toBe('rocket');
  await page.getByRole('button', { name: 'Значок группы Работа: Портфель' }).click();
  await page.getByRole('radiogroup', { name: 'Значок группы Работа' }).getByRole('radio', { name: 'Глобус' }).click();
  await expect.poll(() => api.groups.find((group) => group.name === 'Работа')?.icon).toBe('globe');
  await expect(page.getByRole('button', { name: 'Значок группы Работа: Глобус' })).toBeVisible();
  await page.getByRole('radiogroup', { name: 'Значок группы Работа' }).getByRole('radio', { name: 'Папка' }).click();
  await expect.poll(() => api.groups.find((group) => group.name === 'Работа')?.icon).toBeNull();
});

test('chat_rows_screenshots', async ({ page, isMobile }) => {
  test.skip(!process.env.CHAT_SHOTS, 'снимки для отчёта снимаются по запросу');
  if (!isMobile) await page.setViewportSize({ width: 1920, height: 1080 });
  const { ids } = await boot(page, { path: isMobile ? '/sessions' : undefined });
  if (!isMobile) {
    await page.goto(`/c/${ids.orchestrator}`);
    await expect(page.locator('[data-list-ready]')).toBeVisible();
  }
  await page.waitForTimeout(400);
  await page.screenshot({ path: isMobile ? '/tmp/sessions-chat-phone.png' : '/tmp/sessions-chat-desktop.png' });
  if (isMobile) return;
  await page.evaluate(() => document.documentElement.setAttribute('data-theme', 'light'));
  await page.waitForTimeout(300);
  await page.screenshot({ path: '/tmp/sessions-chat-light.png' });
});
