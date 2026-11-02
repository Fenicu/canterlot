import type { Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup05, seedMockupChat } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

const CATALOG = {
	id: 'workstation',
	name: 'workstation',
	harnesses: [
		{
			id: 'claude',
			title: 'Claude Code',
			provider: 'claude',
			models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
			permission_modes: ['default', 'ask', 'auto_edit', 'full_access']
		}
	]
};

const PLAN = [
	{ text: 'Собрать требования', status: 'completed' as const },
	{ text: 'Написать падающий тест', status: 'in_progress' as const },
	{ text: 'Реализовать панель', status: 'pending' as const }
];

async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	seedMachine(api, CATALOG);
	const id = seedMockupChat(api);
	await installChatRoutes(page, hub, api);
	return { hub, api, id };
}

const panel = (page: Page) => page.getByRole('region', { name: 'Список задач агента' });
const toggle = (page: Page) => panel(page).locator('.toggle');

test('cancelled_item_is_named_in_words_and_leaves_the_progress', async ({ page }) => {
	const { api, id } = await boot(page);
	await page.goto(`/c/${id}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible();

	api.pushTodo(id, [...PLAN, { text: 'Снятый пункт', status: 'cancelled' as const }]);
	await expect(panel(page)).toContainText('1 из 3');
	if ((await toggle(page).getAttribute('aria-expanded')) === 'false') await toggle(page).click();
	const item = panel(page).locator('.item[data-status="cancelled"]');
	await expect(item).toContainText('Снятый пункт');
	const word = item.locator('.word');
	await expect(word).toHaveText('отменено');
	await expect(word).toHaveCSS('color', 'rgb(231, 191, 132)');
});

test('panel_is_absent_without_a_list_and_appears_with_the_first_one', async ({ page }) => {
	const { api, id } = await boot(page);
	await page.goto(`/c/${id}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible();
	await expect(panel(page)).toHaveCount(0);

	api.pushTodo(id, PLAN);
	await expect(panel(page)).toBeVisible();
	await expect(panel(page)).toContainText('1 из 3');
	await expect(panel(page)).toContainText('в работе');
	await expect(panel(page)).toContainText('Написать падающий тест');
});

test('list_shows_items_with_status_words_and_collapses', async ({ page }) => {
	const { api, id } = await boot(page);
	api.pushTodo(id, PLAN);
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toBeVisible();

	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'false');
	await toggle(page).click();

	const items = panel(page).locator('.item');
	await expect(items).toHaveCount(3);
	await expect(items.nth(0)).toContainText('Собрать требования');
	await expect(items.nth(0)).toContainText('готово');
	await expect(items.nth(1)).toContainText('в работе');
	await expect(items.nth(2)).toContainText('ожидает');

	await toggle(page).click();
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'false');
	await expect(items).toHaveCount(0);
});

test('panel_remembers_the_owner_choice_per_device', async ({ page }) => {
	const { api, id } = await boot(page);
	api.pushTodo(id, PLAN);
	await page.goto(`/c/${id}`);
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'false');
	await toggle(page).click();
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'true');
	await page.reload();
	await expect(panel(page)).toContainText('Написать падающий тест');
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'true');
	await toggle(page).click();
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'false');
	await page.reload();
	await expect(panel(page)).toContainText('Написать падающий тест');
	await expect(toggle(page)).toHaveAttribute('aria-expanded', 'false');
});

test('revision_updates_the_same_nodes_in_place', async ({ page }) => {
	const { api, id } = await boot(page);
	api.pushTodo(id, PLAN);
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toBeVisible();
	if ((await toggle(page).getAttribute('aria-expanded')) === 'false') await toggle(page).click();
	const second = panel(page).locator('.item').nth(1);
	await expect(second).toContainText('в работе');
	await second.evaluate((node) => ((node as unknown as { marker: string }).marker = 'same'));

	api.pushTodo(id, [
		{ text: 'Собрать требования', status: 'completed' },
		{ text: 'Написать падающий тест', status: 'completed' },
		{ text: 'Реализовать панель', status: 'in_progress' }
	]);
	await expect(panel(page)).toContainText('2 из 3');
	await expect(second).toContainText('готово');
	expect(await second.evaluate((node) => (node as unknown as { marker?: string }).marker)).toBe('same');
});

test('empty_list_hides_the_panel', async ({ page }) => {
	const { api, id } = await boot(page);
	api.pushTodo(id, PLAN);
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toBeVisible();
	api.pushTodo(id, []);
	await expect(panel(page)).toHaveCount(0);
});

test('finished_list_says_so_and_a_long_one_names_what_did_not_fit', async ({ page }) => {
	const { api, id } = await boot(page);
	api.pushTodo(id, [{ text: 'Один', status: 'completed' }], null, 0);
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toContainText('1 из 1');
	await expect(panel(page)).toContainText('Все пункты выполнены');
	await expect(panel(page).locator('.st')).toHaveAttribute('data-state', 'done');

	api.pushTodo(id, [{ text: 'Первый', status: 'in_progress' }], null, 4);
	await expect(panel(page)).toContainText('0 из 5');
	if ((await toggle(page).getAttribute('aria-expanded')) === 'false') await toggle(page).click();
	await expect(panel(page).locator('.omitted')).toHaveText(/Ещё 4 пункта не поместилось/);
});

test('list_comes_back_from_the_snapshot_after_reload', async ({ page }) => {
	const { api, id } = await boot(page);
	await page.goto(`/c/${id}`);
	api.pushTodo(id, PLAN);
	await expect(panel(page)).toContainText('1 из 3');
	await page.reload();
	await expect(panel(page)).toContainText('1 из 3');
	await expect(panel(page)).toContainText('Написать падающий тест');
});

test('subagent_list_lives_in_its_lane_and_not_in_the_main_chat', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	seedMachine(api, CATALOG);
	const { id, rootBlockId } = seedMockup05(api);
	await installChatRoutes(page, hub, api);
	api.pushTodo(id, [{ text: 'План субагента', status: 'in_progress' }], rootBlockId);

	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();
	await expect(panel(page)).toContainText('План субагента');

	await page.goto(`/c/${id}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible();
	await expect(panel(page)).toHaveCount(0);
});

test('pulse_follows_real_work_and_reduced_motion_stills_it', async ({ page }) => {
	const { api, id } = await boot(page);
	api.details.get(id)!.activity = { state: 'working', evidence: {} };
	api.pushTodo(id, PLAN);
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toBeVisible();
	if ((await toggle(page).getAttribute('aria-expanded')) === 'false') await toggle(page).click();
	const mark = panel(page).locator('.item[data-status="in_progress"] .mark');
	await expect(mark).toBeVisible();
	await expect(panel(page).locator('.st')).toHaveAttribute('data-state', 'active');
	expect(await mark.evaluate((node) => getComputedStyle(node).animationName)).toContain('todo-pulse');

	await page.emulateMedia({ reducedMotion: 'reduce' });
	expect(await mark.evaluate((node) => getComputedStyle(node).animationName)).toBe('none');
});

test('between_turns_the_active_point_rests', async ({ page }) => {
	const { api, id } = await boot(page);
	api.details.get(id)!.activity = { state: 'idle', evidence: {} };
	api.pushTodo(id, PLAN);
	await page.goto(`/c/${id}`);
	await expect(panel(page).locator('.st')).toHaveAttribute('data-state', 'held');
});

test('panel_keeps_to_the_visual_rules', async ({ page }) => {
	const { api, id } = await boot(page);
	const long = 'Очень длинное название пункта, которое не должно растягивать экран телефона никогда'.repeat(3);
	api.pushTodo(id, [...PLAN, { text: long, status: 'pending' }, ...Array.from({ length: 30 }, (_, i) => ({ text: `Пункт ${i}`, status: 'pending' as const }))]);
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toBeVisible();
	if ((await toggle(page).getAttribute('aria-expanded')) === 'false') await toggle(page).click();

	const text = await panel(page).innerText();
	expect(text).not.toMatch(/[✅✓✔✗✘☑\p{Extended_Pictographic}]/u);
	const sizes = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
	expect(sizes.scroll).toBeLessThanOrEqual(sizes.client);
	const list = await panel(page).locator('.items').evaluate((node) => ({ height: node.getBoundingClientRect().height, overflow: getComputedStyle(node).overflowY }));
	expect(list.overflow).toBe('auto');
	expect(list.height).toBeLessThanOrEqual((page.viewportSize()?.height ?? 0) * 0.41);
	const borders = await panel(page).evaluate((root) =>
		[root, ...root.querySelectorAll('*')].filter((node) => parseFloat(getComputedStyle(node).borderTopWidth) > 0).length
	);
	expect(borders).toBe(0);

	const results = await new AxeBuilder({ page }).include('section[aria-label="Список задач агента"]').analyze();
	expect(results.violations).toEqual([]);
});

test('collapsed_panel_stays_one_row_on_the_phone', async ({ page }) => {
	test.skip((page.viewportSize()?.width ?? 0) >= 1024, 'только телефон');
	const { api, id } = await boot(page);
	api.pushTodo(id, Array.from({ length: 40 }, (_, i) => ({ text: `Пункт ${i}`, status: i < 20 ? ('completed' as const) : ('pending' as const) })));
	await page.goto(`/c/${id}`);
	await expect(panel(page)).toBeVisible();
	const box = await panel(page).boundingBox();
	expect(box!.height).toBeLessThanOrEqual(64);
	const track = await panel(page).locator('.st-track').boundingBox();
	const row = await panel(page).locator('.head').boundingBox();
	expect(track!.width).toBeLessThan(row!.width / 2);
});
